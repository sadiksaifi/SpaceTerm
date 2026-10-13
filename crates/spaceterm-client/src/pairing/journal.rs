//! Typed pairing facade over the one client journal. No database or retry loop.

use super::{
    credentials::{
        CredentialCreationReservation, CredentialScope, NativeDeviceCredentials,
        OrdinaryCredentialAuthority, PairingFuture, PreparedCredential,
    },
    revocation::VerifiedRevocationReceipt,
};
use crate::journal::{PairingClientJournal, PairingCommitDisposition};
use spaceterm_core::{
    identity::DeviceId,
    operations::{OperationBinding, OperationId},
    pairing::{
        LocalPairingGeneration, LocalPairingKey, MAX_LOCAL_PAIRING_DECODED_BYTES,
        MAX_PENDING_REVOCATIONS, PairingClientStreams, PairingFailure, PairingKeys,
        PendingRevocationTask, RevocationRouteReference, SignedPairingReceipt,
    },
};
use spaceterm_protocol::authentication::ServerPin;
use std::sync::Arc;

/// Constructed only after verification against the original enrollment/binding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SavedPairing {
    pub(crate) key: LocalPairingKey,
    pub(crate) device: DeviceId,
    pub(crate) pin: ServerPin,
    pub(crate) keys: PairingKeys,
    pub(crate) client: PairingClientStreams,
    pub(crate) credential: PreparedCredential,
    pub(crate) route: RevocationRouteReference,
    pub(crate) receipt: SignedPairingReceipt,
}
impl SavedPairing {
    pub fn key(&self) -> &LocalPairingKey {
        &self.key
    }
    pub fn credential(&self) -> &PreparedCredential {
        &self.credential
    }
    pub fn client(&self) -> &PairingClientStreams {
        &self.client
    }
    pub fn receipt(&self) -> &SignedPairingReceipt {
        &self.receipt
    }
    pub fn pin(&self) -> ServerPin {
        self.pin
    }
}

/// Pre-effect reservation. It owns one cleanup-capacity slot until transferred
/// atomically to enrollment. A missing read never discards this reference:
/// cancellation can leave a native insertion completing in another thread.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CredentialPreparation {
    pub generation: LocalPairingGeneration,
    pub scope: CredentialScope,
    pub credential: Option<PreparedCredential>,
}

/// Public-only crash recovery facts. The single-use bearer is never journaled.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedEnrollment {
    pub local_pairing: LocalPairingKey,
    pub device: DeviceId,
    pub pin: ServerPin,
    pub keys: PairingKeys,
    pub client: spaceterm_core::identity::ClientId,
    pub credential: PreparedCredential,
    pub route: RevocationRouteReference,
    pub enrollment: OperationBinding,
    pub reservation: Option<spaceterm_protocol::pairing_adapter::PublicEnrollmentReservation>,
}

impl PreparedEnrollment {
    pub fn validate(&self) -> Result<(), PairingFailure> {
        let key = self.local_pairing.parts();
        if self.pin.identity != key.server.bytes()
            || self.pin.public_key == [0; 32]
            || self.device.bytes() == [0; 16]
            || self.client.server() != key.server
            || self.enrollment.id.stream().pairing() != key.pairing
            || self.credential.scope.parts().purpose
                != (super::credentials::CredentialPurpose::PairingAuthentication {
                    server: key.server,
                })
            || self.credential.public_key != self.keys.parts().pairing_authentication_public_key
            || self.route.opaque() == [0; 16]
        {
            return Err(PairingFailure::InvalidDurableState);
        }
        if let Some(reservation) = &self.reservation {
            let p = reservation.parts();
            if p.target.parts().pairing != key.pairing
                || p.target.parts().device != self.device
                || p.keys != self.keys
                || p.initial_client != self.client
                || p.enrollment_stream != self.enrollment.id.stream()
                || p.enrollment_request_digest != self.enrollment.digest
                || self.enrollment.id.sequence().get() != 1
            {
                return Err(PairingFailure::InvalidDurableState);
            }
        }
        Ok(())
    }
}

/// Resumable restricted acknowledgement, recorded with the verified binding.
/// No bearer authorization or ordinary authority is retained.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnrollmentAcknowledgementWork {
    pub prepared: PreparedEnrollment,
    pub device: PreparedCredential,
    pub receipt: SignedPairingReceipt,
}
impl EnrollmentAcknowledgementWork {
    pub fn validate(&self) -> Result<(), PairingFailure> {
        self.prepared.validate()?;
        if self.prepared.reservation.is_none()
            || self.device.scope.parts().purpose
                != super::credentials::CredentialPurpose::DeviceIdentity
            || self.device.scope.parts().namespace
                != self.prepared.credential.scope.parts().namespace
            || self.device.public_key != self.prepared.keys.parts().device_identity_public_key
        {
            return Err(PairingFailure::InvalidDurableState);
        }
        super::enrollment::verify_binding(&self.prepared, self.receipt.clone(), false).map(|_| ())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RevocationPhase {
    AwaitingConfirmation,
    ReceiptRecorded {
        receipt: Box<VerifiedRevocationReceipt>,
        acknowledged: bool,
    },
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalRevocation {
    pub task: PendingRevocationTask,
    pub credential: PreparedCredential,
    pub phase: RevocationPhase,
    pub notice_dismissed: bool,
}

/// The provider reserves a task slot for every active pairing incarnation.
/// Removing one binding converts its reservation to a task without new capacity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PairingJournalSnapshot {
    pub revision: u64,
    pub generation_high_water: u64,
    pub credential_preparations: Vec<CredentialPreparation>,
    pub saved: Vec<SavedPairing>,
    pub prepared: Vec<PreparedEnrollment>,
    pub suppressed: Vec<LocalPairingKey>,
    pub revocations: Vec<LocalRevocation>,
    pub enrollment_acknowledgements: Vec<EnrollmentAcknowledgementWork>,
}
impl PairingJournalSnapshot {
    pub fn empty(revision: u64) -> Self {
        Self {
            revision,
            generation_high_water: 0,
            credential_preparations: Vec::new(),
            saved: Vec::new(),
            prepared: Vec::new(),
            suppressed: Vec::new(),
            revocations: Vec::new(),
            enrollment_acknowledgements: Vec::new(),
        }
    }
    pub fn active(&self, key: &LocalPairingKey) -> Option<&SavedPairing> {
        if self.is_suppressed(key) {
            return None;
        }
        self.saved.iter().find(|saved| &saved.key == key)
    }
    pub fn is_suppressed(&self, key: &LocalPairingKey) -> bool {
        self.suppressed.iter().any(|old| {
            old.parts().server == key.parts().server && old.parts().pairing == key.parts().pairing
        })
    }
    pub fn task(&self, operation: OperationId) -> Option<&LocalRevocation> {
        self.revocations
            .iter()
            .find(|v| v.task.parts().original.operation() == operation)
    }
    pub fn reserve_binding(&self, prepared: &PreparedEnrollment) -> Result<(), PairingFailure> {
        prepared.validate()?;
        self.check_credential_owner(&prepared.local_pairing, &prepared.credential)?;
        if let Some(reservation) = self
            .credential_preparations
            .iter()
            .find(|v| v.scope == prepared.credential.scope)
        {
            if reservation.generation != prepared.local_pairing.parts().generation
                || reservation.credential.as_ref() != Some(&prepared.credential)
            {
                return Err(PairingFailure::CredentialIntegrity);
            }
            return Ok(());
        }
        if self
            .prepared
            .iter()
            .any(|old| old.local_pairing == prepared.local_pairing && old.client == prepared.client)
            || self.saved.iter().any(|old| {
                old.key == prepared.local_pairing && old.client.parts().client.id == prepared.client
            })
        {
            return Ok(());
        }
        if self.saved.len()
            + self.prepared.len()
            + self.revocations.len()
            + self.credential_preparations.len()
            >= MAX_PENDING_REVOCATIONS
        {
            return Err(PairingFailure::LimitExceeded);
        }
        Ok(())
    }
    fn next_generation(&self) -> Result<LocalPairingGeneration, PairingFailure> {
        let high = self
            .saved
            .iter()
            .map(|v| v.key.parts().generation.get())
            .chain(
                self.prepared
                    .iter()
                    .map(|v| v.local_pairing.parts().generation.get()),
            )
            .chain(self.suppressed.iter().map(|v| v.parts().generation.get()))
            .fold(self.generation_high_water, u64::max);
        LocalPairingGeneration::new(high.checked_add(1).ok_or(PairingFailure::LimitExceeded)?)
            .map_err(|_| PairingFailure::LimitExceeded)
    }
    fn reserve_credential(
        &self,
        scope: &CredentialScope,
    ) -> Result<CredentialPreparation, PairingFailure> {
        if self.saved.len()
            + self.prepared.len()
            + self.revocations.len()
            + self.credential_preparations.len()
            >= MAX_PENDING_REVOCATIONS
        {
            return Err(PairingFailure::LimitExceeded);
        }
        if self.credential_owners().any(|(_, v)| v.scope == *scope)
            || self
                .credential_preparations
                .iter()
                .any(|v| v.scope == *scope)
        {
            return Err(PairingFailure::CredentialIntegrity);
        }
        let preparation = CredentialPreparation {
            generation: self.next_generation()?,
            scope: scope.clone(),
            credential: None,
        };
        let mut candidate = self.clone();
        candidate.generation_high_water = preparation.generation.get();
        candidate.credential_preparations.push(preparation.clone());
        candidate.validate()?;
        Ok(preparation)
    }
    fn credential_owners(&self) -> impl Iterator<Item = (&LocalPairingKey, &PreparedCredential)> {
        self.saved
            .iter()
            .map(|v| (&v.key, &v.credential))
            .chain(
                self.prepared
                    .iter()
                    .map(|v| (&v.local_pairing, &v.credential)),
            )
            .chain(
                self.revocations
                    .iter()
                    .map(|v| (&v.task.parts().local_pairing, &v.credential)),
            )
            .chain(
                self.enrollment_acknowledgements
                    .iter()
                    .map(|v| (&v.prepared.local_pairing, &v.prepared.credential)),
            )
    }
    pub fn check_credential_owner(
        &self,
        key: &LocalPairingKey,
        credential: &PreparedCredential,
    ) -> Result<(), PairingFailure> {
        if self.credential_preparations.iter().any(|old| {
            old.scope == credential.scope
                && (old.generation != key.parts().generation
                    || old.credential.as_ref() != Some(credential))
        }) {
            return Err(PairingFailure::CredentialIntegrity);
        }
        if self.credential_owners().any(|(old_key, old)| {
            old.scope == credential.scope
                && (old_key != key || old.public_key != credential.public_key)
        }) {
            return Err(PairingFailure::CredentialIntegrity);
        }
        Ok(())
    }
    pub fn validate(&self) -> Result<(), PairingFailure> {
        let record_bytes = (self.saved.len()
            + self.prepared.len()
            + self.revocations.len()
            + self.enrollment_acknowledgements.len()
            + self.credential_preparations.len())
        .checked_mul(4096);
        let decoded_bytes = record_bytes.and_then(|bytes| {
            self.suppressed
                .len()
                .checked_mul(64)
                .and_then(|markers| bytes.checked_add(markers))
        });
        if decoded_bytes.is_none_or(|bytes| bytes > MAX_LOCAL_PAIRING_DECODED_BYTES) {
            return Err(PairingFailure::LimitExceeded);
        }
        for (index, preparation) in self.credential_preparations.iter().enumerate() {
            if preparation.generation.get() > self.generation_high_water
                || preparation
                    .credential
                    .as_ref()
                    .is_some_and(|v| v.scope != preparation.scope || v.public_key == [0; 32])
                || self.credential_preparations[..index].iter().any(|old| {
                    old.generation == preparation.generation || old.scope == preparation.scope
                })
            {
                return Err(PairingFailure::InvalidDurableState);
            }
            for (key, credential) in self
                .credential_owners()
                .filter(|(_, v)| v.scope == preparation.scope)
            {
                if key.parts().generation != preparation.generation
                    || preparation.credential.as_ref() != Some(credential)
                {
                    return Err(PairingFailure::InvalidDurableState);
                }
            }
        }
        if self.enrollment_acknowledgements.len() > MAX_PENDING_REVOCATIONS {
            return Err(PairingFailure::LimitExceeded);
        }
        for (index, work) in self.enrollment_acknowledgements.iter().enumerate() {
            work.validate()?;
            if self.enrollment_acknowledgements[..index]
                .iter()
                .any(|old| old.prepared.enrollment.id == work.prepared.enrollment.id)
            {
                return Err(PairingFailure::InvalidDurableState);
            }
        }
        for (key, credential) in self.credential_owners() {
            self.check_credential_owner(key, credential)
                .map_err(|_| PairingFailure::InvalidDurableState)?;
        }
        for (index, prepared) in self.prepared.iter().enumerate() {
            prepared.validate()?;
            if self.prepared[..index].iter().any(|old| {
                old.enrollment.id == prepared.enrollment.id
                    || (old.local_pairing == prepared.local_pairing
                        && old.client == prepared.client)
            }) {
                return Err(PairingFailure::InvalidDurableState);
            }
        }
        if self
            .saved
            .len()
            .checked_add(self.revocations.len())
            .and_then(|n| n.checked_add(self.prepared.len()))
            .and_then(|n| n.checked_add(self.credential_preparations.len()))
            .is_none_or(|n| n > MAX_PENDING_REVOCATIONS)
        {
            return Err(PairingFailure::LimitExceeded);
        }
        for (index, saved) in self.saved.iter().enumerate() {
            if self.saved[..index].iter().any(|other| {
                other.client.parts().client.id == saved.client.parts().client.id
                    || (other.credential.scope == saved.credential.scope
                        && (other.key != saved.key
                            || other.keys != saved.keys
                            || other.device != saved.device
                            || other.pin != saved.pin))
            }) {
                return Err(PairingFailure::InvalidDurableState);
            }
            super::enrollment::validate_saved(saved)?;
        }
        for (index, pending) in self.revocations.iter().enumerate() {
            let p = pending.task.parts();
            if self.revocations[..index]
                .iter()
                .any(|other| other.task.parts().original.operation() == p.original.operation())
                || !self.suppressed.contains(&p.local_pairing)
                || pending.credential.scope.parts().purpose
                    != (super::credentials::CredentialPurpose::PairingAuthentication {
                        server: p.original.server(),
                    })
                || pending.credential.scope.reference() != p.original.credential()
                || self.active(&p.local_pairing).is_some()
            {
                return Err(PairingFailure::InvalidDurableState);
            }
            if let RevocationPhase::ReceiptRecorded { receipt, .. } = &pending.phase {
                VerifiedRevocationReceipt::verify(
                    &pending.task,
                    &pending.credential,
                    receipt.proof().clone(),
                    receipt.already_revoked(),
                )?;
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PairingJournalChange {
    ReserveCredential(CredentialPreparation),
    RecordCredentialPrepared {
        generation: LocalPairingGeneration,
        credential: PreparedCredential,
    },
    /// Transfers the exact prepared native reservation into enrollment without
    /// allocating another cleanup slot. Public keys must already be durable.
    PrepareEnrollment(Box<PreparedEnrollment>),
    /// Saves a verified additional-Client binding and its task reservation.
    SaveBinding(Box<SavedPairing>),
    /// One transaction activates the binding and retains its outcome ack work.
    SaveEnrollment {
        saved: Box<SavedPairing>,
        acknowledgement: Box<EnrollmentAcknowledgementWork>,
    },
    FinishEnrollmentAcknowledgement {
        operation: OperationId,
    },
    /// Atomically removes ordinary binding, publishes suppression and schedules
    /// restricted work. It never removes a server-owned Workspace or Session.
    SelfUnpair(Box<LocalRevocation>),
    RecordReceipt {
        operation: OperationId,
        receipt: Box<VerifiedRevocationReceipt>,
    },
    RecordAcknowledgement {
        operation: OperationId,
    },
    FinishCleanup {
        operation: OperationId,
        expected_credential: PreparedCredential,
    },
    DismissNotice {
        operation: OperationId,
    },
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PairingJournalTransaction {
    pub expected_revision: u64,
    pub change: PairingJournalChange,
}

pub trait LocalPairingStore: Send + Sync {
    fn ordinary_authority<'a>(
        &'a self,
        key: &'a LocalPairingKey,
    ) -> PairingFuture<'a, Result<OrdinaryCredentialAuthority, PairingFailure>>;
    /// Synchronous runtime latch, before local storage or network I/O.
    fn seal_ordinary(&self, key: &LocalPairingKey);
    fn recover(&self) -> PairingFuture<'_, Result<PairingJournalSnapshot, PairingFailure>>;
    fn commit<'a>(
        &'a self,
        transaction: &'a PairingJournalTransaction,
    ) -> PairingFuture<'a, PairingCommitDisposition>;
    fn dismiss_notice(
        &self,
        expected_revision: u64,
        operation: OperationId,
    ) -> PairingFuture<'_, PairingCommitDisposition> {
        Box::pin(async move {
            self.commit(&PairingJournalTransaction {
                expected_revision,
                change: PairingJournalChange::DismissNotice { operation },
            })
            .await
        })
    }
}

pub struct JournalPairingStore<J: ?Sized> {
    journal: Arc<J>,
}
impl<J: PairingClientJournal + ?Sized> JournalPairingStore<J> {
    pub fn new(journal: Arc<J>) -> Self {
        Self { journal }
    }
}
impl<J: PairingClientJournal + ?Sized> LocalPairingStore for JournalPairingStore<J> {
    fn ordinary_authority<'a>(
        &'a self,
        key: &'a LocalPairingKey,
    ) -> PairingFuture<'a, Result<OrdinaryCredentialAuthority, PairingFailure>> {
        Box::pin(async move {
            let recovered = self.recover().await?;
            let saved = recovered
                .active(key)
                .ok_or(PairingFailure::PairingRevoked)?;
            self.journal
                .pairing_authority_fence()
                .issue(saved.key(), &saved.credential.scope)
        })
    }
    fn seal_ordinary(&self, key: &LocalPairingKey) {
        self.journal.pairing_authority_fence().seal(key);
    }
    fn recover(&self) -> PairingFuture<'_, Result<PairingJournalSnapshot, PairingFailure>> {
        Box::pin(async move {
            let mut recovered = self.journal.recover_pairings().await?;
            recovered.validate()?;
            self.journal.pairing_authority_fence().observe(&recovered)?;
            let suppressed = &recovered.suppressed;
            recovered.saved.retain(|saved| {
                !suppressed.iter().any(|key| {
                    key.parts().server == saved.key.parts().server
                        && key.parts().pairing == saved.key.parts().pairing
                })
            });
            Ok(recovered)
        })
    }
    fn commit<'a>(
        &'a self,
        transaction: &'a PairingJournalTransaction,
    ) -> PairingFuture<'a, PairingCommitDisposition> {
        Box::pin(async move {
            if let PairingJournalChange::SelfUnpair(pending) = &transaction.change {
                self.seal_ordinary(&pending.task.parts().local_pairing);
            }
            let recovered = match self.recover().await {
                Ok(recovered) => recovered,
                Err(failure) => return PairingCommitDisposition::NotApplied(failure),
            };
            if recovered.revision != transaction.expected_revision {
                return PairingCommitDisposition::NotApplied(PairingFailure::StaleAssessment);
            }
            let validation = match &transaction.change {
                PairingJournalChange::ReserveCredential(preparation) => recovered
                    .reserve_credential(&preparation.scope)
                    .and_then(|expected| {
                        if &expected == preparation {
                            Ok(())
                        } else {
                            Err(PairingFailure::InvalidRequest)
                        }
                    }),
                PairingJournalChange::RecordCredentialPrepared {
                    generation,
                    credential,
                } => recovered
                    .credential_preparations
                    .iter()
                    .find(|v| {
                        v.generation == *generation
                            && v.scope == credential.scope
                            && v.credential.as_ref().is_none_or(|old| old == credential)
                    })
                    .map(|_| ())
                    .ok_or(PairingFailure::CredentialIntegrity),
                PairingJournalChange::SaveBinding(saved) => recovered
                    .check_credential_owner(&saved.key, &saved.credential)
                    .and_then(|()| {
                        if recovered.is_suppressed(saved.key()) {
                            return Err(PairingFailure::PairingRevoked);
                        }
                        if !matches!(
                            saved.receipt.parts().claims,
                            spaceterm_core::pairing::PairingReceiptBody::ClientBinding { .. }
                        ) {
                            return Err(PairingFailure::AuthorizationRequired);
                        }
                        super::enrollment::validate_saved(saved)
                    }),
                PairingJournalChange::SaveEnrollment {
                    saved,
                    acknowledgement,
                } => acknowledgement.validate().and_then(|()| {
                    let verified = super::enrollment::verify_binding(
                        &acknowledgement.prepared,
                        acknowledgement.receipt.clone(),
                        false,
                    )?;
                    if saved.as_ref() != &verified {
                        return Err(PairingFailure::IdentityMismatch);
                    }
                    if recovered.is_suppressed(saved.key()) {
                        return Err(PairingFailure::PairingRevoked);
                    }
                    if recovered.enrollment_acknowledgements.len() >= MAX_PENDING_REVOCATIONS {
                        return Err(PairingFailure::LimitExceeded);
                    }
                    recovered.reserve_binding(&acknowledgement.prepared)
                }),
                PairingJournalChange::PrepareEnrollment(prepared) => {
                    if recovered.is_suppressed(&prepared.local_pairing) {
                        Err(PairingFailure::PairingRevoked)
                    } else {
                        recovered.reserve_binding(prepared)
                    }
                }
                PairingJournalChange::SelfUnpair(pending) => recovered.check_credential_owner(
                    &pending.task.parts().local_pairing,
                    &pending.credential,
                ),
                _ => Ok(()),
            };
            if let Err(failure) = validation {
                return PairingCommitDisposition::NotApplied(failure);
            }
            self.journal.commit_pairing(transaction).await
        })
    }
}

pub fn require_durable(
    disposition: PairingCommitDisposition,
    expected_revision: u64,
) -> Result<(), PairingFailure> {
    match disposition {
        PairingCommitDisposition::Durable(receipt)
            if expected_revision.checked_add(1) == Some(receipt.revision().get()) =>
        {
            Ok(())
        }
        PairingCommitDisposition::NotApplied(failure) => Err(failure),
        _ => Err(PairingFailure::RecoveryRequired),
    }
}

/// Commit generation, capacity and exact native reference before any key effect.
pub async fn reserve_native_credential(
    store: &dyn LocalPairingStore,
    scope: CredentialScope,
) -> Result<CredentialCreationReservation, PairingFailure> {
    let recovered = store.recover().await?;
    let preparation = recovered.reserve_credential(&scope)?;
    let generation = preparation.generation;
    let tx = PairingJournalTransaction {
        expected_revision: recovered.revision,
        change: PairingJournalChange::ReserveCredential(preparation),
    };
    require_durable(store.commit(&tx).await, tx.expected_revision)?;
    Ok(CredentialCreationReservation::new(scope, generation))
}
pub async fn record_prepared_credential(
    store: &dyn LocalPairingStore,
    generation: LocalPairingGeneration,
    credential: PreparedCredential,
) -> Result<(), PairingFailure> {
    let recovered = store.recover().await?;
    let tx = PairingJournalTransaction {
        expected_revision: recovered.revision,
        change: PairingJournalChange::RecordCredentialPrepared {
            generation,
            credential,
        },
    };
    require_durable(store.commit(&tx).await, tx.expected_revision)
}
/// Recovery retains absent items as incomplete work. It never creates a key or
/// drops a reference that an interrupted native insertion may still use.
pub async fn recover_native_credential(
    store: &dyn LocalPairingStore,
    native: &dyn NativeDeviceCredentials,
    generation: LocalPairingGeneration,
) -> Result<Option<PreparedCredential>, PairingFailure> {
    let recovered = store.recover().await?;
    let preparation = recovered
        .credential_preparations
        .iter()
        .find(|v| v.generation == generation)
        .ok_or(PairingFailure::InvalidRequest)?;
    let Some(credential) = native.recover_preparation(&preparation.scope).await? else {
        return if preparation.credential.is_some() {
            Err(PairingFailure::CredentialUnavailable)
        } else {
            Ok(None)
        };
    };
    if preparation
        .credential
        .as_ref()
        .is_some_and(|old| old != &credential)
    {
        return Err(PairingFailure::CredentialIntegrity);
    }
    if preparation.credential.is_none() {
        record_prepared_credential(store, generation, credential.clone()).await?;
    }
    Ok(Some(credential))
}

/// Foreground preparation. Both an insertion error and cancellation leave the
/// durable reservation recoverable. Activation cannot precede the second commit.
pub async fn prepare_native_credential(
    store: &dyn LocalPairingStore,
    native: &dyn NativeDeviceCredentials,
    scope: CredentialScope,
) -> Result<(LocalPairingGeneration, PreparedCredential), PairingFailure> {
    let reservation = reserve_native_credential(store, scope).await?;
    let generation = reservation.generation();
    let credential = native.create(reservation).await?;
    record_prepared_credential(store, generation, credential.clone()).await?;
    Ok((generation, credential))
}

/// Native journal uncertainty revokes the same process-local pairing fence.
pub fn seal_journal_pairing(
    fence: &super::credentials::PairingAuthorityFence,
    key: &LocalPairingKey,
) {
    fence.seal(key);
}
