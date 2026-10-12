//! One-shot restricted revocation driver. Client-core owns retry scheduling.

use super::{
    credentials::{
        CredentialDeletion, CredentialGate, CredentialPurpose, CredentialUsePurpose,
        NativeDeviceCredentials, OrdinaryCredentialAuthority, PairingFuture, PreparedCredential,
    },
    enrollment::{PairingTransport, protocol_failure},
    journal::{
        LocalPairingStore, LocalRevocation, PairingJournalChange, PairingJournalTransaction,
        RevocationPhase, require_durable,
    },
};
use sha2::{Digest, Sha256};
use spaceterm_core::{
    operation_ledger_contracts::{CanonicalOperationRequest, LedgerCodec},
    operations::{Command, OperationId, RequestDigest},
    pairing::{
        LocalPairingKey, PairingCommand, PairingFailure, PairingReceiptBody, PairingTarget,
        PairingTargetParts, PendingParts, PendingRevocationTask, PendingRevocationTaskParts,
        PendingSelfRevocation, SignedPairingReceipt,
    },
};
use spaceterm_protocol::{
    authentication::{
        ClientHandshake, Established, HandshakeConfig, Principal, Purpose, ServerPin,
    },
    connection::RestrictedConnection,
    pairing_adapter::{
        PairingRestrictedCodec, ReceiptCodec, operation_from_wire, operation_to_wire,
        target_from_wire, target_to_wire,
    },
    wire::pairing,
};
use std::sync::Arc;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedRevocationReceipt {
    proof: SignedPairingReceipt,
    already_revoked: bool,
}
impl VerifiedRevocationReceipt {
    pub fn verify(
        task: &PendingRevocationTask,
        credential: &PreparedCredential,
        proof: SignedPairingReceipt,
        already_revoked: bool,
    ) -> Result<Self, PairingFailure> {
        let p = task.parts();
        if credential.scope.reference() != p.original.credential()
            || credential.scope.parts().purpose
                != (CredentialPurpose::PairingAuthentication {
                    server: p.original.server(),
                })
        {
            return Err(PairingFailure::IdentityMismatch);
        }
        let claims = ReceiptCodec::new(ServerPin {
            identity: p.original.server().bytes(),
            public_key: p.pinned_server_public_key,
        })
        .verify(&proof)
        .map_err(|_| PairingFailure::ReceiptInvalid)?;
        match claims {
            PairingReceiptBody::Revocation {
                receipt,
                authentication_key_digest,
                ..
            } if receipt.server == p.original.server()
                && receipt.pairing == p.original.pairing()
                && receipt.device == p.original.device()
                && authentication_key_digest
                    == RequestDigest::from_bytes(Sha256::digest(credential.public_key).into())
                && (already_revoked || receipt.operation == p.original.operation()) =>
            {
                Ok(Self {
                    proof,
                    already_revoked,
                })
            }
            _ => Err(PairingFailure::ReceiptInvalid),
        }
    }
    pub fn proof(&self) -> &SignedPairingReceipt {
        &self.proof
    }
    pub fn already_revoked(&self) -> bool {
        self.already_revoked
    }
}

/// Confirmation is tied to the displayed exact local incarnation and stream.
pub struct ConfirmedSelfUnpair {
    key: LocalPairingKey,
    operation: OperationId,
}
impl ConfirmedSelfUnpair {
    pub fn confirm(key: LocalPairingKey, operation: OperationId) -> Result<Self, PairingFailure> {
        if operation.stream().pairing() != key.parts().pairing || operation.sequence().get() != 1 {
            return Err(PairingFailure::IdentityMismatch);
        }
        Ok(Self { key, operation })
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalRemoval {
    pub key: LocalPairingKey,
    pub operation: OperationId,
}

pub struct PairingController<S> {
    store: S,
    codec: Arc<dyn LedgerCodec>,
}
impl<S: LocalPairingStore> PairingController<S> {
    pub fn new(store: S, codec: Arc<dyn LedgerCodec>) -> Self {
        Self { store, codec }
    }
    /// Seal every cloned ordinary lease before the first I/O. A failed or
    /// uncertain local transaction cannot reenable the process's old authority.
    pub async fn self_unpair(
        &self,
        authority: &OrdinaryCredentialAuthority,
        confirmation: ConfirmedSelfUnpair,
    ) -> Result<LocalRemoval, PairingFailure> {
        if authority.key() != &confirmation.key {
            return Err(PairingFailure::StaleAssessment);
        }
        self.store.seal_ordinary(&confirmation.key);
        authority.seal();
        let recovered = self.store.recover().await?;
        if recovered.is_suppressed(&confirmation.key) {
            return Err(PairingFailure::PairingRevoked);
        }
        let saved = recovered
            .saved
            .iter()
            .find(|saved| {
                saved.key() == &confirmation.key
                    && saved.client().parts().self_revocation == confirmation.operation.stream()
            })
            .ok_or(PairingFailure::ClientNotBound)?;
        if saved.client.parts().self_revocation != confirmation.operation.stream() {
            return Err(PairingFailure::IdentityMismatch);
        }
        let target = target(&saved.key, saved.device)?;
        let request = CanonicalOperationRequest::new(
            confirmation.operation,
            Command::Pairing(PairingCommand::SelfRevoke { target }),
            self.codec.as_ref(),
        )
        .map_err(|_| PairingFailure::InvalidRequest)?;
        let original = PendingSelfRevocation::new(PendingParts {
            original_server: saved.key.parts().server,
            original_pairing: saved.key.parts().pairing,
            device: saved.device,
            client: saved.client.parts().client.id,
            operation: confirmation.operation,
            route: saved.route,
            credential: saved.credential.scope.reference(),
            notice_dismissed: false,
        })
        .map_err(|_| PairingFailure::IdentityMismatch)?;
        let task = PendingRevocationTask::new(PendingRevocationTaskParts {
            original,
            local_pairing: saved.key.clone(),
            pinned_server_public_key: saved.pin.public_key,
            request_digest: request.binding().digest,
        })
        .map_err(|_| PairingFailure::IdentityMismatch)?;
        let tx = PairingJournalTransaction {
            expected_revision: recovered.revision,
            change: PairingJournalChange::SelfUnpair(Box::new(LocalRevocation {
                task,
                credential: saved.credential.clone(),
                phase: RevocationPhase::AwaitingConfirmation,
                notice_dismissed: false,
            })),
        };
        require_durable(self.store.commit(&tx).await, tx.expected_revision)?;
        Ok(LocalRemoval {
            key: confirmation.key,
            operation: confirmation.operation,
        })
    }
}
fn target(
    key: &LocalPairingKey,
    device: spaceterm_core::identity::DeviceId,
) -> Result<PairingTarget, PairingFailure> {
    PairingTarget::new(PairingTargetParts {
        server: key.parts().server,
        pairing: key.parts().pairing,
        device,
    })
    .map_err(|_| PairingFailure::IdentityMismatch)
}

/// Captured original route only. Implementations check retained effective SSH
/// host/account facts in batch mode. Alias changes fail with RouteChanged.
/// This port has no bootstrap, Start, update, installation or enrollment method.
pub trait RestrictedConnector: Send + Sync {
    fn open<'a>(
        &'a self,
        task: &'a PendingRevocationTask,
    ) -> PairingFuture<'a, Result<Box<dyn PairingTransport>, PairingFailure>>;
}

type RestrictedTarget = (
    spaceterm_core::identity::ServerId,
    spaceterm_core::identity::PairingId,
);
struct RestrictedAdmission(std::sync::Mutex<[Option<RestrictedTarget>; 2]>);
struct RestrictedAttempt<'a> {
    admission: &'a RestrictedAdmission,
    target: RestrictedTarget,
}
impl RestrictedAdmission {
    fn acquire(
        &self,
        task: &PendingRevocationTask,
    ) -> Result<RestrictedAttempt<'_>, PairingFailure> {
        let target = (
            task.parts().original.server(),
            task.parts().original.pairing(),
        );
        let mut slots = self
            .0
            .lock()
            .map_err(|_| PairingFailure::RecoveryRequired)?;
        if slots.contains(&Some(target)) {
            return Err(PairingFailure::LimitExceeded);
        }
        let slot = slots
            .iter_mut()
            .find(|slot| slot.is_none())
            .ok_or(PairingFailure::LimitExceeded)?;
        *slot = Some(target);
        Ok(RestrictedAttempt {
            admission: self,
            target,
        })
    }
}
impl Drop for RestrictedAttempt<'_> {
    fn drop(&mut self) {
        if let Ok(mut slots) = self.admission.0.lock() {
            for slot in slots.iter_mut() {
                if *slot == Some(self.target) {
                    *slot = None;
                }
            }
        }
    }
}
/// A client runtime installs one driver for all its scheduled tasks.
pub struct RevocationDriver<N, R> {
    credentials: N,
    connector: R,
    config: HandshakeConfig,
    codec: Arc<dyn LedgerCodec>,
    admission: RestrictedAdmission,
}
impl<N: NativeDeviceCredentials, R: RestrictedConnector> RevocationDriver<N, R> {
    pub fn new(
        credentials: N,
        connector: R,
        codec: Arc<dyn LedgerCodec>,
        config: HandshakeConfig,
    ) -> Self {
        Self {
            credentials,
            connector,
            config,
            codec,
            admission: RestrictedAdmission(std::sync::Mutex::new([None; 2])),
        }
    }
    pub async fn resolve(
        &self,
        pending: &LocalRevocation,
    ) -> Result<VerifiedRevocationReceipt, PairingFailure> {
        let _admission = self.admission.acquire(&pending.task)?;
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            let (mut connection, mut transport) = self.connect(pending).await?;
            let p = pending.task.parts();
            let target = target(&p.local_pairing, p.original.device())?;
            let request = pairing::RestrictedExtension {
                kind: Some(pairing::restricted_extension::Kind::Revoke(
                    pairing::SelfRevocation {
                        operation: Some(operation_to_wire(p.original.operation())),
                        request_digest: p.request_digest.bytes().to_vec(),
                        target: Some(target_to_wire(&target)),
                    },
                )),
            };
            let codec = PairingRestrictedCodec::new(pin(&pending.task));
            transport
                .send(
                    &connection
                        .encode_pairing_extension(&request, &codec)
                        .map_err(protocol_failure)?,
                )
                .await?;
            let reply = connection
                .decode_pairing_extension(&transport.receive().await?, &codec)
                .map_err(protocol_failure)?;
            self.verify_response(pending, reply)
        })
        .await
        .map_err(|_| PairingFailure::TimedOut)?
    }
    fn verify_response(
        &self,
        pending: &LocalRevocation,
        reply: pairing::RestrictedExtension,
    ) -> Result<VerifiedRevocationReceipt, PairingFailure> {
        let p = pending.task.parts();
        let result = match reply.kind {
            Some(pairing::restricted_extension::Kind::Result(result)) => result,
            Some(pairing::restricted_extension::Kind::Failure(_)) => {
                return Err(PairingFailure::AuthorizationDenied);
            }
            _ => return Err(PairingFailure::InvalidRequest),
        };
        let operation = operation_from_wire(
            result
                .request_operation
                .as_ref()
                .ok_or(PairingFailure::ReceiptInvalid)?,
        )
        .map_err(protocol_failure)?;
        let reply_target = target_from_wire(
            result
                .target
                .as_ref()
                .ok_or(PairingFailure::ReceiptInvalid)?,
        )
        .map_err(protocol_failure)?;
        if operation != p.original.operation()
            || result.request_digest != p.request_digest.bytes()
            || reply_target != target(&p.local_pairing, p.original.device())?
        {
            return Err(PairingFailure::ReceiptInvalid);
        }
        if result.disposition == 3 {
            return Err(PairingFailure::RevocationUnconfirmed);
        }
        if !matches!(result.disposition, 1 | 2) {
            return Err(PairingFailure::ReceiptInvalid);
        }
        let proof = ReceiptCodec::new(pin(&pending.task))
            .from_wire(
                result
                    .receipt
                    .as_ref()
                    .ok_or(PairingFailure::ReceiptInvalid)?,
            )
            .map_err(|_| PairingFailure::ReceiptInvalid)?;
        VerifiedRevocationReceipt::verify(
            &pending.task,
            &pending.credential,
            proof,
            result.disposition == 2,
        )
    }
    async fn connect(
        &self,
        pending: &LocalRevocation,
    ) -> Result<(RestrictedConnection, Box<dyn PairingTransport>), PairingFailure> {
        let p = pending.task.parts();
        let command = Command::Pairing(PairingCommand::SelfRevoke {
            target: target(&p.local_pairing, p.original.device())?,
        });
        let request =
            CanonicalOperationRequest::new(p.original.operation(), command, self.codec.as_ref())
                .map_err(|_| PairingFailure::InvalidRequest)?;
        if request.binding().digest != p.request_digest
            || p.original.credential() != pending.credential.scope.reference()
        {
            return Err(PairingFailure::IdentityMismatch);
        }
        let gate = CredentialGate::new(pending.credential.scope.clone());
        let lease = gate.lease(CredentialUsePurpose::Restricted {
            operation: p.original.operation(),
        });
        let signer = self
            .credentials
            .signing_key(&pending.credential, &lease)
            .await?;
        let mut transport = self.connector.open(&pending.task).await?;
        let local = |value: u64| {
            let mut bytes = [0; 16];
            bytes[8..].copy_from_slice(&value.to_be_bytes());
            bytes
        };
        let principal = Principal {
            server: p.original.server().bytes(),
            device: p.original.device().bytes(),
            pairing: local(p.original.pairing().local()),
            client: local(p.original.client().local()),
        };
        let (handshake, hello) = ClientHandshake::begin(
            pin(&pending.task),
            principal,
            Purpose::RevocationStatus,
            self.config.clone(),
            signer,
        )
        .map_err(protocol_failure)?;
        transport.send(&hello).await?;
        let (ready, proof) = handshake
            .receive_server(&transport.receive().await?)
            .map_err(protocol_failure)?;
        transport.send(&proof).await?;
        match ready
            .receive_ready(&transport.receive().await?)
            .map_err(protocol_failure)?
        {
            Established::Restricted(connection) => Ok((connection, transport)),
            Established::Ordinary(_) => Err(PairingFailure::AuthorizationDenied),
        }
    }
    /// Receipt recording is a prerequisite. A following status round trip keeps
    /// the credential until the serialized peer has processed acknowledgement.
    pub async fn acknowledge(&self, pending: &LocalRevocation) -> Result<(), PairingFailure> {
        let _admission = self.admission.acquire(&pending.task)?;
        if !matches!(pending.phase, RevocationPhase::ReceiptRecorded { .. }) {
            return Err(PairingFailure::InvalidDurableState);
        }
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            let (mut connection, mut transport) = self.connect(pending).await?;
            let p = pending.task.parts();
            let target = target_to_wire(&target(&p.local_pairing, p.original.device())?);
            let codec = PairingRestrictedCodec::new(pin(&pending.task));
            let ack = pairing::RestrictedExtension {
                kind: Some(pairing::restricted_extension::Kind::Acknowledgement(
                    pairing::RestrictedAcknowledgement {
                        operation: Some(operation_to_wire(p.original.operation())),
                        request_digest: p.request_digest.bytes().to_vec(),
                        target: Some(target.clone()),
                        acknowledged_through: 1,
                    },
                )),
            };
            transport
                .send(
                    &connection
                        .encode_pairing_extension(&ack, &codec)
                        .map_err(protocol_failure)?,
                )
                .await?;
            let status = pairing::RestrictedExtension {
                kind: Some(pairing::restricted_extension::Kind::Status(
                    pairing::ReceiptStatus {
                        operation: Some(operation_to_wire(p.original.operation())),
                        request_digest: p.request_digest.bytes().to_vec(),
                        target: Some(target),
                    },
                )),
            };
            transport
                .send(
                    &connection
                        .encode_pairing_extension(&status, &codec)
                        .map_err(protocol_failure)?,
                )
                .await?;
            let reply = connection
                .decode_pairing_extension(&transport.receive().await?, &codec)
                .map_err(protocol_failure)?;
            self.verify_response(pending, reply).map(|_| ())
        })
        .await
        .map_err(|_| PairingFailure::TimedOut)?
    }
    async fn cleanup(
        &self,
        pending: &LocalRevocation,
    ) -> Result<CredentialDeletion, PairingFailure> {
        let RevocationPhase::ReceiptRecorded {
            receipt,
            acknowledged: true,
        } = &pending.phase
        else {
            return Err(PairingFailure::InvalidDurableState);
        };
        VerifiedRevocationReceipt::verify(
            &pending.task,
            &pending.credential,
            receipt.proof().clone(),
            receipt.already_revoked(),
        )?;
        self.credentials
            .delete_pairing_key(&pending.credential)
            .await
    }
    /// Executes one scheduled attempt. This has no clock, backoff or task loop.
    /// Every phase is reread from the one journal after its durable transaction.
    pub async fn run_once(
        &self,
        store: &dyn LocalPairingStore,
        operation: OperationId,
    ) -> Result<(), PairingFailure> {
        let recovered = store.recover().await?;
        let pending = recovered
            .task(operation)
            .ok_or(PairingFailure::InvalidRequest)?;
        if matches!(pending.phase, RevocationPhase::AwaitingConfirmation) {
            let receipt = self.resolve(pending).await?;
            let tx = PairingJournalTransaction {
                expected_revision: recovered.revision,
                change: PairingJournalChange::RecordReceipt {
                    operation,
                    receipt: Box::new(receipt),
                },
            };
            require_durable(store.commit(&tx).await, tx.expected_revision)?;
        }
        let recovered = store.recover().await?;
        let pending = recovered
            .task(operation)
            .ok_or(PairingFailure::InvalidDurableState)?;
        if matches!(
            pending.phase,
            RevocationPhase::ReceiptRecorded {
                acknowledged: false,
                ..
            }
        ) {
            self.acknowledge(pending).await?;
            let tx = PairingJournalTransaction {
                expected_revision: recovered.revision,
                change: PairingJournalChange::RecordAcknowledgement { operation },
            };
            require_durable(store.commit(&tx).await, tx.expected_revision)?;
        }
        let recovered = store.recover().await?;
        let pending = recovered
            .task(operation)
            .ok_or(PairingFailure::InvalidDurableState)?;
        if recovered
            .enrollment_acknowledgements
            .iter()
            .any(|work| work.prepared.credential.scope == pending.credential.scope)
        {
            return Err(PairingFailure::RecoveryRequired);
        }
        self.cleanup(pending).await?;
        let tx = PairingJournalTransaction {
            expected_revision: recovered.revision,
            change: PairingJournalChange::FinishCleanup {
                operation,
                expected_credential: pending.credential.clone(),
            },
        };
        require_durable(store.commit(&tx).await, tx.expected_revision)
    }
}
fn pin(task: &PendingRevocationTask) -> ServerPin {
    ServerPin {
        identity: task.parts().original.server().bytes(),
        public_key: task.parts().pinned_server_public_key,
    }
}
