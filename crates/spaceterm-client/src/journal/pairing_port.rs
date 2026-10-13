//! Pairing is a projection of the aggregate journal, including its revision CAS.

use super::*;
use crate::pairing::{
    credentials::{
        CredentialNamespace, CredentialPurpose, CredentialScope, CredentialScopeParts,
        PreparedCredential,
    },
    journal::{
        CredentialPreparation, EnrollmentAcknowledgementWork, LocalRevocation,
        PairingJournalChange, PreparedEnrollment, RevocationPhase as PairingPhase, SavedPairing,
    },
    revocation::VerifiedRevocationReceipt,
};
use spaceterm_protocol::pairing_adapter::PublicEnrollmentReservation;

pub(super) fn failure(error: ClientFailure) -> PairingFailure {
    match error.class() {
        ClientFailureClass::ResourceLimit | ClientFailureClass::CounterExhausted => {
            PairingFailure::LimitExceeded
        }
        ClientFailureClass::RecoveryRequired => PairingFailure::RecoveryRequired,
        ClientFailureClass::StorageUnavailable => PairingFailure::StorageUnavailable,
        ClientFailureClass::UnsafeStorage => PairingFailure::UnsafeStorage,
        ClientFailureClass::UnknownSchema => PairingFailure::UnknownSchema,
        ClientFailureClass::AuthorizationDenied => PairingFailure::AuthorizationDenied,
        _ => PairingFailure::InvalidDurableState,
    }
}
impl JournalCredentialSlot {
    fn scope(&self) -> Result<CredentialScope, PairingFailure> {
        CredentialScope::new(CredentialScopeParts {
            namespace: CredentialNamespace::new(self.namespace.clone())?,
            purpose: self
                .server
                .map_or(CredentialPurpose::DeviceIdentity, |server| {
                    CredentialPurpose::PairingAuthentication { server }
                }),
            reference: self.reference,
        })
    }
    fn prepared(&self) -> Result<PreparedCredential, PairingFailure> {
        Ok(PreparedCredential {
            scope: self.scope()?,
            public_key: self.public_key.ok_or(PairingFailure::CredentialIntegrity)?,
        })
    }
}
impl RecoveredClientState {
    pub fn pairing_snapshot(&self) -> Result<PairingJournalSnapshot, PairingFailure> {
        let credential = |reference| {
            self.pairing
                .slot(reference)
                .ok_or(PairingFailure::CredentialIntegrity)?
                .prepared()
        };
        let saved: Vec<SavedPairing> = self
            .bindings
            .iter()
            .map(|saved| {
                let target = saved.target.target().map_err(failure)?;
                let keys = match &saved.proof.parts().claims {
                    PairingReceiptBody::Enrollment { keys, .. }
                    | PairingReceiptBody::ClientBinding { keys, .. } => keys.clone(),
                    _ => return Err(PairingFailure::InvalidDurableState),
                };
                Ok(SavedPairing {
                    key: target.local_pairing().map_err(failure)?,
                    device: target.device(),
                    pin: target.pin(),
                    keys,
                    client: saved.streams.clone(),
                    credential: credential(target.credential())?,
                    route: self
                        .pairing
                        .slot(target.credential())
                        .and_then(|slot| slot.route_reference)
                        .ok_or(PairingFailure::InvalidDurableState)?,
                    receipt: saved.proof.clone(),
                })
            })
            .collect::<Result<_, _>>()?;
        let preparations = self
            .pairing
            .credential_preparations
            .iter()
            .map(|reference| {
                let slot = self
                    .pairing
                    .slot(*reference)
                    .ok_or(PairingFailure::CredentialIntegrity)?;
                Ok(CredentialPreparation {
                    generation: slot.generation,
                    scope: slot.scope()?,
                    credential: slot.public_key.map(|_| slot.prepared()).transpose()?,
                })
            })
            .collect::<Result<_, PairingFailure>>()?;
        let revocations = self.revocations.iter().map(|record| {
            let credential = credential(record.target.credential)?;
            let phase = match &record.phase {
                RevocationPhase::AwaitingConfirmation => PairingPhase::AwaitingConfirmation,
                RevocationPhase::CleanupPending(receipt) => {
                    let already_revoked = matches!(&receipt.parts().claims, PairingReceiptBody::Revocation { receipt, .. } if receipt.operation != record.task.parts().original.operation());
                    PairingPhase::ReceiptRecorded {
                        receipt: Box::new(VerifiedRevocationReceipt::verify(&record.task, &credential, *receipt.clone(), already_revoked)?),
                        acknowledged: self.stream(record.task.parts().original.operation().stream()).is_some_and(|stream| stream.confirmed_prefix == 1),
                    }
                }
            };
            Ok(LocalRevocation { task: record.task.clone(), credential, phase, notice_dismissed: record.notice_dismissed })
        }).collect::<Result<_, PairingFailure>>()?;
        let additional_client_acknowledgements = self
            .bindings
            .iter()
            .filter(|binding| binding.additional_client_acknowledgement_pending())
            .map(|pending| {
                saved
                    .iter()
                    .find(|saved| saved.receipt() == pending.proof())
                    .cloned()
                    .ok_or(PairingFailure::InvalidDurableState)
            })
            .collect::<Result<_, _>>()?;
        let snapshot = PairingJournalSnapshot {
            additional_client_acknowledgements,
            revision: self.revision.get(),
            generation_high_water: self.pairing.generation_high_water(),
            credential_preparations: preparations,
            saved,
            prepared: self
                .pairing
                .prepared()
                .iter()
                .map(|prepared| self.project_enrollment(prepared))
                .collect::<Result<_, _>>()?,
            suppressed: self.tombstones.clone(),
            revocations,
            enrollment_acknowledgements: self
                .pairing
                .acknowledgements()
                .iter()
                .map(|ack| {
                    Ok(EnrollmentAcknowledgementWork {
                        prepared: self.project_enrollment(&ack.prepared)?,
                        device: credential(ack.device_credential)?,
                        receipt: ack.receipt.clone(),
                    })
                })
                .collect::<Result<_, PairingFailure>>()?,
        };
        snapshot.validate()?;
        Ok(snapshot)
    }
    fn project_enrollment(
        &self,
        prepared: &JournalEnrollment,
    ) -> Result<PreparedEnrollment, PairingFailure> {
        let target = prepared.target.target().map_err(failure)?;
        let reservation = prepared
            .reservation
            .as_ref()
            .map(|reservation| {
                use spaceterm_protocol::{
                    pairing_adapter::{operation_to_wire, target_to_wire},
                    wire::{common, pairing},
                };
                let server = common::Identity {
                    value: target.server().bytes().to_vec(),
                };
                let pairing_target = spaceterm_core::pairing::PairingTarget::new(
                    spaceterm_core::pairing::PairingTargetParts {
                        server: target.server(),
                        pairing: target.pairing(),
                        device: target.device(),
                    },
                )
                .map_err(|_| PairingFailure::InvalidDurableState)?;
                PublicEnrollmentReservation::from_wire(&pairing::Reservation {
                    ticket: Some(pairing::Ticket {
                        server: Some(server.clone()),
                        ticket_nonce: reservation.ticket.parts().nonce.to_vec(),
                    }),
                    target: Some(target_to_wire(&pairing_target)),
                    keys: Some(pairing::Keys {
                        device_identity_public_key: prepared
                            .keys
                            .parts()
                            .device_identity_public_key
                            .to_vec(),
                        pairing_authentication_public_key: prepared
                            .keys
                            .parts()
                            .pairing_authentication_public_key
                            .to_vec(),
                    }),
                    initial_client: Some(common::Entity {
                        server: Some(server),
                        local_id: target.client().local(),
                    }),
                    enrollment_operation: Some(operation_to_wire(prepared.enrollment.id)),
                    claim_digest: reservation.claim_digest.bytes().to_vec(),
                    enrollment_request_digest: prepared.enrollment.digest.bytes().to_vec(),
                })
                .map_err(|_| PairingFailure::InvalidDurableState)
            })
            .transpose()?;
        Ok(PreparedEnrollment {
            local_pairing: target.local_pairing().map_err(failure)?,
            device: target.device(),
            pin: target.pin(),
            keys: prepared.keys.clone(),
            client: target.client(),
            credential: self
                .pairing
                .slot(target.credential())
                .ok_or(PairingFailure::CredentialIntegrity)?
                .prepared()?,
            route: prepared.route_reference,
            enrollment: prepared.enrollment,
            reservation,
        })
    }
    fn route(
        &self,
        reference: CredentialReference,
        route_reference: spaceterm_core::pairing::RevocationRouteReference,
    ) -> Result<RouteDescription, PairingFailure> {
        let slot = self
            .pairing
            .slot(reference)
            .ok_or(PairingFailure::CredentialIntegrity)?;
        if slot.route_reference != Some(route_reference) {
            return Err(PairingFailure::IdentityMismatch);
        }
        slot.route
            .clone()
            .or_else(|| {
                self.pairing
                    .prepared()
                    .iter()
                    .chain(
                        self.pairing
                            .acknowledgements()
                            .iter()
                            .map(|ack| &ack.prepared),
                    )
                    .find(|prepared| prepared.target.credential == reference)
                    .map(|prepared| prepared.target.route.clone())
            })
            .or_else(|| {
                self.bindings
                    .iter()
                    .find(|binding| binding.target.credential == reference)
                    .map(|binding| binding.target.route.clone())
            })
            .ok_or(PairingFailure::AuthorizationRequired)
    }
    fn retain_enrollment(
        &self,
        prepared: &PreparedEnrollment,
    ) -> Result<JournalEnrollment, PairingFailure> {
        prepared.validate()?;
        let target = ConnectionTarget::new(
            prepared.pin,
            prepared.device,
            prepared.local_pairing.parts().pairing,
            prepared.client,
            self.route(prepared.credential.scope.reference(), prepared.route)?,
            prepared.credential.scope.reference(),
            prepared.local_pairing.parts().generation,
        )
        .map_err(failure)?;
        Ok(JournalEnrollment {
            target: SavedTarget::from_target(&target),
            keys: prepared.keys.clone(),
            route_reference: prepared.route,
            enrollment: prepared.enrollment,
            reservation: prepared.reservation.as_ref().map(|reservation| {
                JournalEnrollmentReservation {
                    ticket: reservation.parts().ticket.clone(),
                    claim_digest: reservation.parts().claim_digest,
                }
            }),
        })
    }
    fn retain_binding(&self, saved: &SavedPairing) -> Result<SavedBinding, PairingFailure> {
        crate::pairing::enrollment::validate_saved(saved)?;
        let target = ConnectionTarget::new(
            saved.pin,
            saved.device,
            saved.key.parts().pairing,
            saved.client.parts().client.id,
            self.route(saved.credential.scope.reference(), saved.route)?,
            saved.credential.scope.reference(),
            saved.key.parts().generation,
        )
        .map_err(failure)?;
        Ok(SavedBinding {
            target: SavedTarget::from_target(&target),
            streams: saved.client.clone(),
            proof: saved.receipt.clone(),
            additional_client_acknowledgement_pending: false,
        })
    }
    fn pairing_transaction(
        &self,
        tx: &PairingJournalTransaction,
    ) -> Result<ClientJournalTransaction, PairingFailure> {
        if self.revision.get() != tx.expected_revision {
            return Err(PairingFailure::StaleAssessment);
        }
        let changes = match &tx.change {
            PairingJournalChange::ReserveCredential(preparation) => {
                if preparation.credential.is_some() {
                    return Err(PairingFailure::InvalidRequest);
                }
                vec![ClientJournalChange::ReserveCredential(
                    JournalCredentialSlot {
                        namespace: preparation.scope.parts().namespace.as_str().to_owned(),
                        server: match preparation.scope.parts().purpose {
                            CredentialPurpose::DeviceIdentity => None,
                            CredentialPurpose::PairingAuthentication { server } => Some(server),
                        },
                        reference: preparation.scope.reference(),
                        public_key: None,
                        route_reference: None,
                        route: None,
                        ssh_capture: None,
                        generation: preparation.generation,
                    },
                )]
            }
            PairingJournalChange::RecordCredentialPrepared {
                generation,
                credential,
            } => {
                let slot = self
                    .pairing
                    .slot(credential.scope.reference())
                    .ok_or(PairingFailure::CredentialIntegrity)?;
                if slot.generation != *generation || slot.scope()? != credential.scope {
                    return Err(PairingFailure::CredentialIntegrity);
                }
                vec![ClientJournalChange::RecordCredentialPublicKey {
                    reference: credential.scope.reference(),
                    public_key: credential.public_key,
                }]
            }
            PairingJournalChange::PrepareEnrollment(prepared) => {
                vec![ClientJournalChange::PrepareEnrollment(Box::new(
                    self.retain_enrollment(prepared)?,
                ))]
            }
            PairingJournalChange::SaveBinding(saved) => {
                if !matches!(
                    saved.receipt.parts().claims,
                    PairingReceiptBody::ClientBinding { .. }
                ) {
                    return Err(PairingFailure::AuthorizationRequired);
                }
                vec![ClientJournalChange::SaveBinding(Box::new(
                    self.retain_binding(saved)?
                        .await_additional_client_acknowledgement(),
                ))]
            }
            PairingJournalChange::SaveEnrollment {
                saved,
                acknowledgement,
            } => {
                acknowledgement.validate()?;
                let verified = crate::pairing::enrollment::verify_binding(
                    &acknowledgement.prepared,
                    acknowledgement.receipt.clone(),
                    false,
                )?;
                if saved.as_ref() != &verified {
                    return Err(PairingFailure::IdentityMismatch);
                }
                vec![ClientJournalChange::SaveEnrollment {
                    binding: Box::new(self.retain_binding(saved)?),
                    acknowledgement: Box::new(JournalEnrollmentAcknowledgement {
                        prepared: self.retain_enrollment(&acknowledgement.prepared)?,
                        device_credential: acknowledgement.device.scope.reference(),
                        receipt: acknowledgement.receipt.clone(),
                    }),
                }]
            }
            PairingJournalChange::FinishEnrollmentAcknowledgement { operation } => {
                vec![ClientJournalChange::FinishEnrollmentAcknowledgement(
                    *operation,
                )]
            }
            PairingJournalChange::SelfUnpair(pending) => {
                if pending.phase != PairingPhase::AwaitingConfirmation || pending.notice_dismissed {
                    return Err(PairingFailure::InvalidRequest);
                }
                let original = &pending.task.parts().original;
                let saved = self
                    .bindings
                    .iter()
                    .find(|saved| {
                        saved.target.credential == original.credential()
                            && saved.target.client == original.client()
                    })
                    .ok_or(PairingFailure::ClientNotBound)?;
                if self
                    .pairing
                    .slot(original.credential())
                    .ok_or(PairingFailure::CredentialIntegrity)?
                    .prepared()?
                    != pending.credential
                {
                    return Err(PairingFailure::CredentialIntegrity);
                }
                vec![ClientJournalChange::BeginRevocation(Box::new(
                    JournalRevocation {
                        task: pending.task.clone(),
                        target: saved.target.clone(),
                        phase: RevocationPhase::AwaitingConfirmation,
                        notice_dismissed: false,
                        attempts: 0,
                    },
                ))]
            }
            PairingJournalChange::RecordReceipt { operation, receipt } => {
                let record = self
                    .revocations
                    .iter()
                    .find(|record| record.task.parts().original.operation() == *operation)
                    .ok_or(PairingFailure::InvalidRequest)?;
                VerifiedRevocationReceipt::verify(
                    &record.task,
                    &self
                        .pairing
                        .slot(record.target.credential)
                        .ok_or(PairingFailure::CredentialIntegrity)?
                        .prepared()?,
                    receipt.proof().clone(),
                    receipt.already_revoked(),
                )?;
                vec![ClientJournalChange::ConfirmRevocation {
                    operation: *operation,
                    receipt: Box::new(receipt.proof().clone()),
                }]
            }
            PairingJournalChange::RecordAcknowledgement { operation } => {
                vec![ClientJournalChange::ConfirmRevocationAcknowledgement(
                    *operation,
                )]
            }
            PairingJournalChange::FinishCleanup {
                operation,
                expected_credential,
            } => {
                let record = self
                    .revocations
                    .iter()
                    .find(|record| record.task.parts().original.operation() == *operation)
                    .ok_or(PairingFailure::InvalidRequest)?;
                if self
                    .pairing
                    .slot(record.target.credential)
                    .ok_or(PairingFailure::CredentialIntegrity)?
                    .prepared()?
                    != *expected_credential
                {
                    return Err(PairingFailure::CredentialIntegrity);
                }
                vec![ClientJournalChange::FinishCredentialCleanup(*operation)]
            }
            PairingJournalChange::DismissNotice { operation } => {
                vec![ClientJournalChange::DismissRevocation(*operation)]
            }
        };
        Ok(ClientJournalTransaction {
            expected_revision: self.revision,
            changes,
        })
    }
}

impl<J: ClientJournal + ?Sized> PairingClientJournal for J {
    fn pairing_authority_fence(&self) -> &PairingAuthorityFence {
        self.authority_fence()
    }
    fn recover_pairings(
        &self,
    ) -> PairingFuture<'_, Result<PairingJournalSnapshot, PairingFailure>> {
        Box::pin(async move {
            let snapshot = self.recover().await.map_err(failure)?.pairing_snapshot()?;
            self.authority_fence().observe(&snapshot)?;
            Ok(snapshot)
        })
    }
    fn commit_pairing<'a>(
        &'a self,
        transaction: &'a PairingJournalTransaction,
    ) -> PairingFuture<'a, PairingCommitDisposition> {
        Box::pin(async move {
            if let PairingJournalChange::SelfUnpair(pending) = &transaction.change {
                self.authority_fence()
                    .seal(&pending.task.parts().local_pairing);
            }
            let state = match self.recover().await {
                Ok(state) => state,
                Err(error) => return PairingCommitDisposition::NotApplied(failure(error)),
            };
            let tx = match state.pairing_transaction(transaction) {
                Ok(tx) => tx,
                Err(error) => return PairingCommitDisposition::NotApplied(error),
            };
            match self.commit(&tx).await {
                Ok(JournalCommitDisposition::Durable(receipt)) => {
                    // Canonical writes and pairing writes both advance this one revision.
                    // Observe through fresh recovery so suppression wins before return.
                    match self.recover_pairings().await {
                        Ok(_) => PairingCommitDisposition::Durable(receipt),
                        Err(_) => PairingCommitDisposition::Indeterminate,
                    }
                }
                Ok(JournalCommitDisposition::NotApplied(error)) => {
                    PairingCommitDisposition::NotApplied(failure(error.into()))
                }
                _ => {
                    for saved in state.bindings() {
                        if let Ok(target) = saved
                            .target
                            .target()
                            .and_then(|target| target.local_pairing())
                        {
                            self.authority_fence().seal(&target);
                        }
                    }
                    PairingCommitDisposition::Indeterminate
                }
            }
        })
    }
}
