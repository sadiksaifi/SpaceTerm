//! Bootstrap enrollment reuses native key custody, the one client journal and the
//! purpose-restricted enrollment driver. Owner lookup never issues a new native request.

#[cfg(test)]
mod tests;

use std::sync::Arc;

use ring::digest::{Context, SHA256};
use spaceterm_client::pairing::{
    bootstrap::{
        BootstrapAttempt, BootstrapEnrollment, BootstrapFailure, BootstrapPhase, BootstrapRoute,
        BootstrapServer,
    },
    credentials::{
        CredentialNamespace, CredentialPurpose, NativeDeviceCredentials, PairingFuture,
        PreparedCredential,
    },
    enrollment::{
        DirectEnrollmentAttempt, EnrollmentAttemptRequest, EnrollmentConnector, EnrollmentDriver,
    },
    journal::{
        JournalPairingStore, LocalPairingStore, PreparedEnrollment, SavedPairing,
        prepare_native_credential, recover_native_credential,
    },
};
use spaceterm_core::{
    identity::{DeviceId, ServerId},
    operations::{OperationBinding, OperationId, OperationSequence, RequestDigest},
    pairing::{
        EnrollmentProposal, EnrollmentProposalParts, LocalPairingGeneration, LocalPairingKey,
        LocalPairingKeyParts, PairingFailure, PairingKeys, PairingKeysParts,
        RevocationRouteReference,
    },
};
use spaceterm_protocol::pairing_adapter::{
    EnrollmentAuthorization, PublicEnrollmentReservation, enrollment_proposal_digest,
};

use crate::{credentials::adapter::DeviceCredentials, journal::NativeClientJournal};

/// Transient owner delivery. The bearer stays in zeroized protocol storage and is never journaled.
pub struct BootstrapOwnerDelivery {
    pub native_request: [u8; 16],
    pub native_request_digest: RequestDigest,
    pub reservation: PublicEnrollmentReservation,
    pub authorization: Option<EnrollmentAuthorization>,
}

/// The retained SSH account-owner entry and the application's ordinary client connection.
/// Lookup must read the exact previously issued native request, not reenroll on a new ID.
pub trait BootstrapOwnerExchange: Send + Sync {
    fn lookup<'a>(
        &'a self,
        server: &'a BootstrapServer,
        request: [u8; 16],
        proposal: &'a EnrollmentProposal,
    ) -> PairingFuture<'a, Result<BootstrapOwnerDelivery, PairingFailure>>;
    fn lookup_verified<'a>(
        &'a self,
        server: &'a BootstrapServer,
        request: [u8; 16],
        proposal: &'a EnrollmentProposal,
        _device_proof: [u8; 64],
        _authentication_proof: [u8; 64],
    ) -> PairingFuture<'a, Result<BootstrapOwnerDelivery, PairingFailure>> {
        self.lookup(server, request, proposal)
    }
    fn prepare_binding<'a>(
        &'a self,
        _prepared: &'a PreparedEnrollment,
    ) -> PairingFuture<'a, Result<(), PairingFailure>> {
        Box::pin(async { Ok(()) })
    }
    fn connect<'a>(
        &'a self,
        server: &'a BootstrapServer,
        saved: &'a SavedPairing,
    ) -> PairingFuture<'a, Result<(), PairingFailure>>;
}

/// Foreground-only production enrollment with native credentials and the canonical SQLite journal.
/// Namespace and route reference are explicit local composition, not protocol-supplied authority.
pub struct NativeBootstrapEnrollment<R, O, J = NativeClientJournal, C = DeviceCredentials> {
    journal: Arc<J>,
    credentials: C,
    connector: R,
    owner: O,
    namespace: CredentialNamespace,
    route: BootstrapRoute,
    reference: RevocationRouteReference,
    preparation: tokio::sync::Mutex<()>,
    connections: std::sync::Mutex<Vec<(BootstrapAttempt, LocalPairingKey)>>,
    completed: std::sync::Mutex<Option<(PreparedEnrollment, SavedPairing)>>,
}
impl<
    R: EnrollmentConnector + Clone,
    O: BootstrapOwnerExchange,
    J: spaceterm_client::journal::PairingClientJournal,
    C: NativeDeviceCredentials + Clone,
> NativeBootstrapEnrollment<R, O, J, C>
{
    pub fn new(
        journal: Arc<J>,
        credentials: C,
        connector: R,
        owner: O,
        namespace: CredentialNamespace,
        route: BootstrapRoute,
        reference: RevocationRouteReference,
    ) -> Result<Self, PairingFailure> {
        if reference.opaque() == [0; 16] {
            return Err(PairingFailure::InvalidRequest);
        }
        Ok(Self {
            journal,
            credentials,
            connector,
            owner,
            namespace,
            route,
            reference,
            preparation: tokio::sync::Mutex::new(()),
            connections: std::sync::Mutex::new(Vec::new()),
            completed: std::sync::Mutex::new(None),
        })
    }
    /// Returns the exact durable enrollment completed by this foreground coordinator.
    pub fn completed(&self) -> Result<Option<(PreparedEnrollment, SavedPairing)>, PairingFailure> {
        self.completed
            .lock()
            .map(|slot| slot.clone())
            .map_err(|_| PairingFailure::RecoveryRequired)
    }
    fn store(&self) -> JournalPairingStore<J> {
        JournalPairingStore::new(Arc::clone(&self.journal))
    }
    fn check(
        &self,
        route: BootstrapRoute,
        attempt: &BootstrapAttempt,
        phase: BootstrapPhase,
    ) -> Result<(), BootstrapFailure> {
        attempt.validate(phase)?;
        if route != self.route {
            return Err(BootstrapFailure::new(PairingFailure::RouteChanged, phase));
        }
        Ok(())
    }
    async fn credential(
        &self,
        purpose: CredentialPurpose,
    ) -> Result<(LocalPairingGeneration, PreparedCredential), PairingFailure> {
        let store = self.store();
        let recovered = store.recover().await?;
        if let CredentialPurpose::PairingAuthentication { server } = purpose
            && let Some(prepared) = recovered.prepared.iter().find(|p| {
                p.pin.identity == server.bytes()
                    && !recovered.is_suppressed(&p.local_pairing)
                    && p.credential.scope.parts().namespace == self.namespace
            })
        {
            let actual = self
                .credentials
                .recover_preparation(&prepared.credential.scope)
                .await?
                .ok_or(PairingFailure::CredentialUnavailable)?;
            if actual != prepared.credential {
                return Err(PairingFailure::CredentialIntegrity);
            }
            return Ok((prepared.local_pairing.parts().generation, actual));
        }
        let reservation = recovered.credential_preparations.iter().find(|p| {
            p.scope.parts().namespace == self.namespace
                && p.scope.parts().purpose == purpose
                && (purpose == CredentialPurpose::DeviceIdentity
                    || !recovered
                        .saved
                        .iter()
                        .any(|saved| saved.credential().scope == p.scope))
        });
        if let Some(reservation) = reservation {
            let credential =
                recover_native_credential(&store, &self.credentials, reservation.generation)
                    .await?
                    .ok_or(PairingFailure::RecoveryRequired)?;
            return Ok((reservation.generation, credential));
        }
        let scope = DeviceCredentials::new_scope(self.namespace.clone(), purpose)?;
        prepare_native_credential(&store, &self.credentials, scope).await
    }
    async fn original_candidate(
        &self,
        server: &BootstrapServer,
        expected: &EnrollmentProposal,
    ) -> Result<
        (
            LocalPairingGeneration,
            PreparedCredential,
            PreparedCredential,
            EnrollmentProposal,
        ),
        PairingFailure,
    > {
        let (_, device) = self.credential(CredentialPurpose::DeviceIdentity).await?;
        let recovered = self.store().recover().await?;
        let candidates = recovered
            .saved
            .iter()
            .map(|saved| (saved.key(), saved.credential()))
            .chain(
                recovered
                    .prepared
                    .iter()
                    .map(|prepared| (&prepared.local_pairing, &prepared.credential)),
            );
        for (key, authentication) in candidates {
            if key.parts().server.bytes() != server.pin.identity
                || authentication.scope.parts().namespace != self.namespace
            {
                continue;
            }
            let proposal = proposal(server, &device, authentication)?;
            if &proposal != expected {
                continue;
            }
            if recovered.is_suppressed(key) {
                return Err(PairingFailure::PairingRevoked);
            }
            let native = self
                .credentials
                .recover_preparation(&authentication.scope)
                .await?
                .ok_or(PairingFailure::CredentialUnavailable)?;
            if &native != authentication {
                return Err(PairingFailure::CredentialIntegrity);
            }
            return Ok((key.parts().generation, device, native, proposal));
        }
        let candidate = self.candidate(server).await?;
        if &candidate.3 != expected {
            return Err(PairingFailure::IdentityMismatch);
        }
        Ok(candidate)
    }
    async fn candidate(
        &self,
        server: &BootstrapServer,
    ) -> Result<
        (
            LocalPairingGeneration,
            PreparedCredential,
            PreparedCredential,
            EnrollmentProposal,
        ),
        PairingFailure,
    > {
        let (_, device) = self.credential(CredentialPurpose::DeviceIdentity).await?;
        let (generation, authentication) = self
            .credential(CredentialPurpose::PairingAuthentication {
                server: ServerId::new(server.pin.identity),
            })
            .await?;
        let proposal = proposal(server, &device, &authentication)?;
        Ok((generation, device, authentication, proposal))
    }
}
impl<
    R: EnrollmentConnector + Clone + Send + Sync,
    O: BootstrapOwnerExchange,
    J: spaceterm_client::journal::PairingClientJournal,
    C: NativeDeviceCredentials + Clone,
> BootstrapEnrollment for NativeBootstrapEnrollment<R, O, J, C>
{
    fn preflight<'a>(
        &'a self,
        route: BootstrapRoute,
        attempt: &'a BootstrapAttempt,
    ) -> PairingFuture<'a, Result<(), BootstrapFailure>> {
        Box::pin(async move {
            self.check(route, attempt, BootstrapPhase::ReadOnlyPreflight)?;
            let _guard = self.preparation.lock().await;
            self.credential(CredentialPurpose::DeviceIdentity)
                .await
                .map_err(|e| BootstrapFailure::new(e, BootstrapPhase::ReadOnlyPreflight))?;
            self.check(route, attempt, BootstrapPhase::ReadOnlyPreflight)
        })
    }
    fn prepare_owner<'a>(
        &'a self,
        server: &'a BootstrapServer,
        attempt: &'a BootstrapAttempt,
    ) -> PairingFuture<'a, Result<EnrollmentProposal, BootstrapFailure>> {
        Box::pin(async move {
            self.check(server.route, attempt, BootstrapPhase::Planned)?;
            let _guard = self.preparation.lock().await;
            let (_, _, _, proposal) = self
                .candidate(server)
                .await
                .map_err(|e| BootstrapFailure::new(e, BootstrapPhase::Planned))?;
            self.check(server.route, attempt, BootstrapPhase::Planned)?;
            Ok(proposal)
        })
    }
    fn enroll_owner<'a>(
        &'a self,
        server: &'a BootstrapServer,
        request: [u8; 16],
        original: &'a EnrollmentProposal,
        attempt: &'a BootstrapAttempt,
    ) -> PairingFuture<'a, Result<(), BootstrapFailure>> {
        Box::pin(async move {
            self.check(server.route, attempt, BootstrapPhase::Applied)?;
            let _guard = self.preparation.lock().await;
            let (generation, device, authentication, proposal) = self
                .original_candidate(server, original)
                .await
                .map_err(|e| BootstrapFailure::new(e, BootstrapPhase::Enrolled))?;
            if &proposal != original || request == [0; 16] {
                return Err(BootstrapFailure::new(
                    PairingFailure::IdentityMismatch,
                    BootstrapPhase::Enrolled,
                ));
            }
            let comparison =
                DirectEnrollmentAttempt::new(server.pin, self.reference, attempt.generation())
                    .map_err(|e| BootstrapFailure::new(e, BootstrapPhase::Enrolled))?;
            let verified = comparison
                .confirm(
                    server.pin,
                    self.reference,
                    attempt.generation(),
                    &comparison.fingerprint(),
                )
                .map_err(|e| BootstrapFailure::new(e, BootstrapPhase::Enrolled))?;
            let device_signer = self
                .credentials
                .signing_key(
                    &device,
                    &verified
                        .credential_use(&device)
                        .map_err(|e| BootstrapFailure::new(e, BootstrapPhase::Enrolled))?,
                )
                .await
                .map_err(|e| BootstrapFailure::new(e, BootstrapPhase::Enrolled))?;
            let auth_signer = self
                .credentials
                .signing_key(
                    &authentication,
                    &verified
                        .credential_use(&authentication)
                        .map_err(|e| BootstrapFailure::new(e, BootstrapPhase::Enrolled))?,
                )
                .await
                .map_err(|e| BootstrapFailure::new(e, BootstrapPhase::Enrolled))?;
            let (device_proof, authentication_proof) =
                spaceterm_protocol::pairing_adapter::VerifiedDeviceClaim::proofs(
                    &proposal,
                    device_signer.as_ref(),
                    auth_signer.as_ref(),
                )
                .map_err(|_| {
                    BootstrapFailure::new(
                        PairingFailure::AuthorizationDenied,
                        BootstrapPhase::Enrolled,
                    )
                })?;
            let delivery = self
                .owner
                .lookup_verified(
                    server,
                    request,
                    &proposal,
                    device_proof,
                    authentication_proof,
                )
                .await
                .map_err(|e| BootstrapFailure::new(e, BootstrapPhase::Enrolled))?;
            self.check(server.route, attempt, BootstrapPhase::Enrolled)?;
            let reservation = delivery.reservation.parts();
            let claim_digest = enrollment_proposal_digest(&proposal).map_err(|_| {
                BootstrapFailure::new(PairingFailure::InvalidRequest, BootstrapPhase::Enrolled)
            })?;
            if delivery.native_request != request
                || delivery.native_request_digest.bytes() == [0; 32]
                || reservation.claim_digest != claim_digest
                || reservation.target.parts().server.bytes() != server.pin.identity
                || reservation.target.parts().device != proposal.parts().device
                || reservation.keys != proposal.parts().keys
            {
                return Err(BootstrapFailure::new(
                    PairingFailure::IdentityMismatch,
                    BootstrapPhase::Enrolled,
                ));
            }
            let prepared = PreparedEnrollment {
                local_pairing: LocalPairingKey::new(LocalPairingKeyParts {
                    server: ServerId::new(server.pin.identity),
                    pairing: reservation.target.parts().pairing,
                    generation,
                })
                .map_err(|_| {
                    BootstrapFailure::new(PairingFailure::InvalidRequest, BootstrapPhase::Enrolled)
                })?,
                device: proposal.parts().device,
                pin: server.pin,
                keys: proposal.parts().keys.clone(),
                client: reservation.initial_client,
                credential: authentication,
                route: self.reference,
                enrollment: OperationBinding {
                    id: OperationId::new(
                        reservation.enrollment_stream,
                        OperationSequence::new(1).map_err(|_| {
                            BootstrapFailure::new(
                                PairingFailure::InvalidRequest,
                                BootstrapPhase::Enrolled,
                            )
                        })?,
                    ),
                    digest: reservation.enrollment_request_digest,
                },
                reservation: Some(delivery.reservation.clone()),
            };
            prepared
                .validate()
                .map_err(|e| BootstrapFailure::new(e, BootstrapPhase::Enrolled))?;
            // This confirmation belongs only to deliberate Bootstrap after OpenSSH account trust.
            // Ordinary enrollment still requires the caller's out-of-band fingerprint comparison.
            let comparison =
                DirectEnrollmentAttempt::new(server.pin, self.reference, attempt.generation())
                    .map_err(|e| BootstrapFailure::new(e, BootstrapPhase::Enrolled))?;
            let verified = comparison
                .confirm(
                    server.pin,
                    self.reference,
                    attempt.generation(),
                    &comparison.fingerprint(),
                )
                .map_err(|e| BootstrapFailure::new(e, BootstrapPhase::Enrolled))?;
            self.owner
                .prepare_binding(&prepared)
                .await
                .map_err(|e| BootstrapFailure::new(e, BootstrapPhase::Enrolled))?;
            let driver = EnrollmentDriver::new(
                self.store(),
                self.credentials.clone(),
                self.connector.clone(),
            );
            let saved = driver
                .enroll(EnrollmentAttemptRequest {
                    verified: &verified,
                    prepared: &prepared,
                    reservation: delivery.reservation,
                    device: &device,
                    authorization: delivery.authorization.as_ref(),
                    majors: server
                        .protocol_majors
                        .iter()
                        .rev()
                        .copied()
                        .filter(|major| matches!(major, 1 | 2))
                        .map(u32::from)
                        .collect(),
                })
                .await
                .map_err(|e| BootstrapFailure::new(e, BootstrapPhase::Enrolled))?;
            *self.completed.lock().map_err(|_| {
                BootstrapFailure::new(PairingFailure::RecoveryRequired, BootstrapPhase::Enrolled)
            })? = Some((prepared, saved.clone()));
            self.check(server.route, attempt, BootstrapPhase::Enrolled)?;
            let mut connections = self.connections.lock().map_err(|_| {
                BootstrapFailure::new(PairingFailure::RecoveryRequired, BootstrapPhase::Enrolled)
            })?;
            connections.retain(|(original, _)| {
                original.validate(BootstrapPhase::Enrolled).is_ok() && original != attempt
            });
            if connections.len() >= 4 {
                return Err(BootstrapFailure::new(
                    PairingFailure::LimitExceeded,
                    BootstrapPhase::Enrolled,
                ));
            }
            connections.push((attempt.clone(), saved.key().clone()));
            Ok(())
        })
    }
    fn connect<'a>(
        &'a self,
        server: &'a BootstrapServer,
        attempt: &'a BootstrapAttempt,
    ) -> PairingFuture<'a, Result<(), BootstrapFailure>> {
        Box::pin(async move {
            self.check(server.route, attempt, BootstrapPhase::Connected)?;
            let mut recovered = self
                .store()
                .recover()
                .await
                .map_err(|e| BootstrapFailure::new(e, BootstrapPhase::Connected))?;
            let key = self
                .connections
                .lock()
                .map_err(|_| {
                    BootstrapFailure::new(
                        PairingFailure::RecoveryRequired,
                        BootstrapPhase::Connected,
                    )
                })?
                .iter()
                .find(|(original, _)| original == attempt)
                .map(|(_, key)| key.clone())
                .ok_or_else(|| {
                    BootstrapFailure::new(
                        PairingFailure::AuthorizationDenied,
                        BootstrapPhase::Connected,
                    )
                })?;
            if let Some(pending) = recovered
                .enrollment_acknowledgements
                .iter()
                .find(|pending| pending.prepared.local_pairing == key)
            {
                // A saved receipt does not settle its acknowledgement. Resume
                // only that historical exchange, never another enrollment.
                EnrollmentDriver::new(
                    self.store(),
                    self.credentials.clone(),
                    self.connector.clone(),
                )
                .acknowledge_once(
                    pending.prepared.enrollment.id,
                    server
                        .protocol_majors
                        .iter()
                        .rev()
                        .copied()
                        .filter(|major| matches!(major, 1 | 2))
                        .map(u32::from)
                        .collect(),
                )
                .await
                .map_err(|e| BootstrapFailure::new(e, BootstrapPhase::Connected))?;
                self.check(server.route, attempt, BootstrapPhase::Connected)?;
                recovered = self
                    .store()
                    .recover()
                    .await
                    .map_err(|e| BootstrapFailure::new(e, BootstrapPhase::Connected))?;
                if recovered
                    .enrollment_acknowledgements
                    .iter()
                    .any(|pending| pending.prepared.local_pairing == key)
                {
                    return Err(BootstrapFailure::new(
                        PairingFailure::RecoveryRequired,
                        BootstrapPhase::Connected,
                    ));
                }
            }
            let saved = recovered
                .active(&key)
                .filter(|saved| saved.pin() == server.pin)
                .ok_or_else(|| {
                    BootstrapFailure::new(
                        PairingFailure::AuthorizationDenied,
                        BootstrapPhase::Connected,
                    )
                })?;
            self.owner
                .connect(server, saved)
                .await
                .map_err(|e| BootstrapFailure::new(e, BootstrapPhase::Connected))?;
            self.check(server.route, attempt, BootstrapPhase::Connected)?;
            self.connections
                .lock()
                .map_err(|_| {
                    BootstrapFailure::new(
                        PairingFailure::RecoveryRequired,
                        BootstrapPhase::Connected,
                    )
                })?
                .retain(|(original, _)| original != attempt);
            Ok(())
        })
    }
}

fn proposal(
    server: &BootstrapServer,
    device: &PreparedCredential,
    authentication: &PreparedCredential,
) -> Result<EnrollmentProposal, PairingFailure> {
    let mut identity = Context::new(&SHA256);
    identity.update(b"spaceterm-bootstrap-device-v1\0");
    identity.update(device.scope.parts().namespace.as_str().as_bytes());
    identity.update(&device.public_key);
    let mut device_id = [0; 16];
    device_id.copy_from_slice(&identity.finish().as_ref()[..16]);
    let mut nonce = Context::new(&SHA256);
    nonce.update(b"spaceterm-bootstrap-owner-claim-v1\0");
    nonce.update(&server.pin.identity);
    nonce.update(&server.pin.public_key);
    nonce.update(&device.scope.reference().opaque());
    nonce.update(&authentication.scope.reference().opaque());
    let mut claim_nonce = [0; 32];
    claim_nonce.copy_from_slice(nonce.finish().as_ref());
    let keys = PairingKeys::new(PairingKeysParts {
        device_identity_public_key: device.public_key,
        pairing_authentication_public_key: authentication.public_key,
    })
    .map_err(|_| PairingFailure::InvalidRequest)?;
    EnrollmentProposal::new(EnrollmentProposalParts {
        server: ServerId::new(server.pin.identity),
        device: DeviceId::new(device_id),
        keys,
        claim_nonce,
    })
    .map_err(|_| PairingFailure::InvalidRequest)
}
