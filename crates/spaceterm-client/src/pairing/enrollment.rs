//! Foreground pin verification, purpose-restricted enrollment and durable binding.

use super::{
    credentials::{
        CredentialGate, CredentialPurpose, CredentialUse, CredentialUsePurpose,
        NativeDeviceCredentials, PairingFuture, PreparedCredential,
    },
    journal::{
        EnrollmentAcknowledgementWork, LocalPairingStore, PairingJournalChange,
        PairingJournalTransaction, PreparedEnrollment, SavedPairing, require_durable,
    },
};
use sha2::{Digest, Sha256};
use spaceterm_core::pairing::{
    PairingFailure, PairingReceiptBody, RevocationRouteReference, SignedPairingReceipt,
};
use spaceterm_protocol::{
    ProtocolError,
    authentication::{ServerPin, SigningKey},
    pairing_adapter::{
        EnrollmentAuthorization, EnrollmentClient, PublicEnrollmentReservation, ReceiptCodec,
    },
    wire::pairing,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU8, Ordering},
};
use zeroize::Zeroizing;

/// Candidate data has no connection or credential authority.
pub struct DirectEnrollmentAttempt {
    pin: ServerPin,
    route: RevocationRouteReference,
    generation: u64,
    valid: Arc<AtomicBool>,
    authorization_attempts: Arc<AtomicU8>,
}
impl DirectEnrollmentAttempt {
    pub fn new(
        pin: ServerPin,
        route: RevocationRouteReference,
        generation: u64,
    ) -> Result<Self, PairingFailure> {
        if pin.identity == [0; 16]
            || pin.public_key == [0; 32]
            || route.opaque() == [0; 16]
            || generation == 0
        {
            return Err(PairingFailure::InvalidRequest);
        }
        Ok(Self {
            pin,
            route,
            generation,
            valid: Arc::new(AtomicBool::new(true)),
            authorization_attempts: Arc::new(AtomicU8::new(0)),
        })
    }
    pub fn fingerprint(&self) -> String {
        fingerprint(self.pin.public_key)
    }
    /// The UI calls this only after explicit out-of-band comparison. Exact
    /// candidate, route and attempt stamps reject delayed confirmation actions.
    pub fn confirm(
        &self,
        pin: ServerPin,
        route: RevocationRouteReference,
        generation: u64,
        compared_fingerprint: &str,
    ) -> Result<VerifiedServer, PairingFailure> {
        if !self.valid.load(Ordering::Acquire)
            || pin != self.pin
            || route != self.route
            || generation != self.generation
            || compared_fingerprint != self.fingerprint()
        {
            return Err(PairingFailure::FingerprintMismatch);
        }
        Ok(VerifiedServer {
            pin,
            route,
            valid: self.valid.clone(),
            authorization_attempts: self.authorization_attempts.clone(),
        })
    }
    pub fn cancel(&self) {
        self.valid.store(false, Ordering::Release);
    }
}
impl Drop for DirectEnrollmentAttempt {
    fn drop(&mut self) {
        self.cancel();
    }
}

pub struct VerifiedServer {
    pin: ServerPin,
    route: RevocationRouteReference,
    valid: Arc<AtomicBool>,
    authorization_attempts: Arc<AtomicU8>,
}
impl VerifiedServer {
    pub fn pin(&self) -> ServerPin {
        self.pin
    }
    pub fn credential_use(
        &self,
        credential: &PreparedCredential,
    ) -> Result<CredentialUse, PairingFailure> {
        self.validate()?;
        match credential.scope.parts().purpose {
            CredentialPurpose::DeviceIdentity => (),
            CredentialPurpose::PairingAuthentication { server }
                if server.bytes() == self.pin.identity => {}
            _ => return Err(PairingFailure::IdentityMismatch),
        }
        Ok(CredentialUse::enrollment(
            credential.scope.clone(),
            self.valid.clone(),
        ))
    }
    fn validate(&self) -> Result<(), PairingFailure> {
        if !self.valid.load(Ordering::Acquire) {
            return Err(PairingFailure::Cancelled);
        }
        Ok(())
    }
}

pub fn fingerprint(key: [u8; 32]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let hash = Sha256::digest(key);
    let mut result = String::with_capacity(50);
    result.push_str("SHA256:");
    for chunk in hash.chunks(3) {
        let bits = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for index in 0..chunk.len() + 1 {
            result.push(ALPHABET[((bits >> (18 - index * 6)) & 63) as usize] as char);
        }
    }
    result
}

/// Restricted reachability only. No discovery, management, bootstrap or repair.
pub trait PairingTransport: Send {
    fn send<'a>(&'a mut self, bytes: &'a [u8]) -> PairingFuture<'a, Result<(), PairingFailure>>;
    fn receive(&mut self) -> PairingFuture<'_, Result<Zeroizing<Vec<u8>>, PairingFailure>>;
}
pub trait EnrollmentConnector: Send + Sync {
    fn open<'a>(
        &'a self,
        pin: ServerPin,
        route: RevocationRouteReference,
    ) -> PairingFuture<'a, Result<Box<dyn PairingTransport>, PairingFailure>>;
}

pub fn protocol_failure(error: ProtocolError) -> PairingFailure {
    match error {
        ProtocolError::Incompatible | ProtocolError::Negotiation => PairingFailure::Incompatible,
        ProtocolError::PairingDenied | ProtocolError::Authentication => {
            PairingFailure::AuthorizationDenied
        }
        ProtocolError::SigningUnavailable => PairingFailure::CredentialUnavailable,
        ProtocolError::LimitExceeded => PairingFailure::LimitExceeded,
        _ => PairingFailure::InvalidRequest,
    }
}

/// Verifies every local and server binding before creating an ordinary journal entry.
pub fn verify_binding(
    prepared: &PreparedEnrollment,
    proof: SignedPairingReceipt,
    additional_client: bool,
) -> Result<SavedPairing, PairingFailure> {
    let claims = ReceiptCodec::new(prepared.pin)
        .verify(&proof)
        .map_err(|_| PairingFailure::ReceiptInvalid)?;
    let (target, keys, client, binding) = match claims {
        PairingReceiptBody::Enrollment {
            target,
            keys,
            client,
            enrollment,
            ..
        } if !additional_client => (target, keys, client, enrollment),
        PairingReceiptBody::ClientBinding {
            target,
            keys,
            client,
            operation,
            ..
        } if additional_client => (target, keys, client, operation),
        _ => return Err(PairingFailure::ReceiptInvalid),
    };
    if target.parts().server != prepared.local_pairing.parts().server
        || target.parts().pairing != prepared.local_pairing.parts().pairing
        || target.parts().device != prepared.device
        || prepared.pin.identity != target.parts().server.bytes()
        || keys != prepared.keys
        || client.parts().client.id != prepared.client
        || binding != prepared.enrollment
        || prepared.credential.scope.parts().purpose
            != (CredentialPurpose::PairingAuthentication {
                server: target.parts().server,
            })
        || prepared.credential.public_key != keys.parts().pairing_authentication_public_key
        || prepared.route.opaque() == [0; 16]
    {
        return Err(PairingFailure::IdentityMismatch);
    }
    Ok(SavedPairing {
        key: prepared.local_pairing.clone(),
        device: prepared.device,
        pin: prepared.pin,
        keys,
        client,
        credential: prepared.credential.clone(),
        route: prepared.route,
        receipt: proof,
    })
}

pub(crate) fn validate_saved(saved: &SavedPairing) -> Result<(), PairingFailure> {
    let additional = matches!(
        saved.receipt.parts().claims,
        PairingReceiptBody::ClientBinding { .. }
    );
    let operation = match &saved.receipt.parts().claims {
        PairingReceiptBody::Enrollment { enrollment, .. } => *enrollment,
        PairingReceiptBody::ClientBinding { operation, .. } => *operation,
        _ => return Err(PairingFailure::InvalidDurableState),
    };
    verify_binding(
        &PreparedEnrollment {
            local_pairing: saved.key.clone(),
            device: saved.device,
            pin: saved.pin,
            keys: saved.keys.clone(),
            client: saved.client.parts().client.id,
            credential: saved.credential.clone(),
            route: saved.route,
            enrollment: operation,
            reservation: None,
        },
        saved.receipt.clone(),
        additional,
    )
    .map(|_| ())
}

/// Save an explicitly authorized additional-Client receipt through the same
/// facade. Initial enrollment uses the driver's atomic binding/ack transaction.
/// A historical receipt alone does not reactivate suppression.
pub async fn save_binding(
    store: &dyn LocalPairingStore,
    prepared: &PreparedEnrollment,
    proof: SignedPairingReceipt,
    additional_client: bool,
) -> Result<SavedPairing, PairingFailure> {
    prepared.validate()?;
    let saved = verify_binding(prepared, proof, additional_client)?;
    let recovered = store.recover().await?;
    if recovered.is_suppressed(saved.key()) {
        return Err(PairingFailure::PairingRevoked);
    }
    recovered.reserve_binding(prepared)?;
    if !additional_client {
        return Err(PairingFailure::AuthorizationRequired);
    }
    if recovered.saved.contains(&saved) {
        return Ok(saved);
    }
    let tx = PairingJournalTransaction {
        expected_revision: recovered.revision,
        change: PairingJournalChange::SaveBinding(Box::new(saved.clone())),
    };
    require_durable(store.commit(&tx).await, tx.expected_revision)?;
    Ok(saved)
}

pub struct EnrollmentAttemptRequest<'a> {
    pub verified: &'a VerifiedServer,
    pub prepared: &'a PreparedEnrollment,
    pub reservation: PublicEnrollmentReservation,
    pub device: &'a PreparedCredential,
    pub authorization: Option<&'a EnrollmentAuthorization>,
    pub majors: Vec<u32>,
}
pub struct EnrollmentDriver<S, N, R> {
    store: S,
    credentials: N,
    connector: R,
    admission: tokio::sync::Semaphore,
}
impl<S: LocalPairingStore, N: NativeDeviceCredentials, R: EnrollmentConnector>
    EnrollmentDriver<S, N, R>
{
    pub fn new(store: S, credentials: N, connector: R) -> Self {
        Self {
            store,
            credentials,
            connector,
            admission: tokio::sync::Semaphore::new(4),
        }
    }
    /// The original reservation and native reference must be retained before this
    /// call. A lost response retries lookup on this exact identity, not new issuance.
    pub async fn enroll(
        &self,
        request: EnrollmentAttemptRequest<'_>,
    ) -> Result<SavedPairing, PairingFailure> {
        let EnrollmentAttemptRequest {
            verified,
            prepared,
            reservation,
            device,
            authorization,
            majors,
        } = request;
        let _admission = self
            .admission
            .try_acquire()
            .map_err(|_| PairingFailure::LimitExceeded)?;
        verified.validate()?;
        let p = reservation.parts();
        if verified.pin != prepared.pin
            || verified.route != prepared.route
            || p.target.parts().pairing != prepared.local_pairing.parts().pairing
            || p.target.parts().device != prepared.device
            || p.keys != prepared.keys
            || p.initial_client != prepared.client
            || p.enrollment_stream != prepared.enrollment.id.stream()
            || p.enrollment_request_digest != prepared.enrollment.digest
            || device.scope.parts().purpose != CredentialPurpose::DeviceIdentity
            || device.public_key != prepared.keys.parts().device_identity_public_key
            || device.scope.parts().namespace != prepared.credential.scope.parts().namespace
        {
            return Err(PairingFailure::IdentityMismatch);
        }
        if authorization.is_some()
            && verified
                .authorization_attempts
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                    (count < 3).then_some(count + 1)
                })
                .is_err()
        {
            return Err(PairingFailure::AuthorizationRequired);
        }
        let recovered = self.store.recover().await?;
        verified.validate()?;
        if recovered.is_suppressed(&prepared.local_pairing) {
            return Err(PairingFailure::PairingRevoked);
        }
        let mut retained = prepared.clone();
        retained.reservation = Some(reservation.clone());
        recovered.reserve_binding(&retained)?;
        if let Some(original) = recovered
            .prepared
            .iter()
            .find(|old| old.enrollment.id == retained.enrollment.id)
        {
            if original != &retained {
                return Err(PairingFailure::RequestIdentityReused);
            }
        } else {
            if authorization.is_none() {
                return Err(PairingFailure::AuthorizationRequired);
            }
            let tx = PairingJournalTransaction {
                expected_revision: recovered.revision,
                change: PairingJournalChange::PrepareEnrollment(Box::new(retained.clone())),
            };
            require_durable(self.store.commit(&tx).await, tx.expected_revision)?;
        }
        let device_lease = verified.credential_use(device)?;
        let auth_lease = verified.credential_use(&prepared.credential)?;
        let device_signer = self.credentials.signing_key(device, &device_lease).await?;
        let auth_signer = self
            .credentials
            .signing_key(&prepared.credential, &auth_lease)
            .await?;
        let (result, mut connection, mut transport) = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            self.exchange(
                verified,
                reservation,
                device_signer,
                auth_signer,
                authorization,
                majors,
            ),
        )
        .await
        .map_err(|_| PairingFailure::TimedOut)??;
        verified.validate()?;
        let saved = verify_binding(&retained, result.clone(), false)?;
        let work = EnrollmentAcknowledgementWork {
            prepared: retained,
            device: device.clone(),
            receipt: result,
        };
        work.validate()?;
        let recovered = self.store.recover().await?;
        verified.validate()?;
        if recovered.is_suppressed(saved.key()) {
            return Err(PairingFailure::PairingRevoked);
        }
        recovered.reserve_binding(&work.prepared)?;
        if !recovered.saved.contains(&saved) {
            let tx = PairingJournalTransaction {
                expected_revision: recovered.revision,
                change: PairingJournalChange::SaveEnrollment {
                    saved: Box::new(saved.clone()),
                    acknowledgement: Box::new(work.clone()),
                },
            };
            require_durable(self.store.commit(&tx).await, tx.expected_revision)?;
        }
        // Binding activation is already durable. Offline/lost acknowledgement
        // remains scheduled in the same journal rather than undoing enrollment.
        let _pending = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            self.acknowledge_on(&work, &mut connection, transport.as_mut()),
        )
        .await;
        Ok(saved)
    }
    async fn exchange(
        &self,
        verified: &VerifiedServer,
        reservation: PublicEnrollmentReservation,
        device: Arc<dyn SigningKey>,
        auth: Arc<dyn SigningKey>,
        authorization: Option<&EnrollmentAuthorization>,
        majors: Vec<u32>,
    ) -> Result<
        (
            SignedPairingReceipt,
            spaceterm_protocol::pairing_adapter::EnrollmentConnection,
            Box<dyn PairingTransport>,
        ),
        PairingFailure,
    > {
        let mut transport = self.connector.open(verified.pin, verified.route).await?;
        let (client, hello) =
            EnrollmentClient::begin(verified.pin, reservation, majors, device.clone(), auth)
                .map_err(protocol_failure)?;
        transport.send(&hello).await?;
        let (ready, proof) = client
            .receive_server(&transport.receive().await?)
            .map_err(protocol_failure)?;
        verified.validate()?;
        transport.send(&proof).await?;
        let mut connection = ready
            .receive_ready(&transport.receive().await?)
            .map_err(protocol_failure)?;
        let bytes = match authorization {
            Some(authorization) => connection
                .submission_message(authorization, device.as_ref())
                .map_err(protocol_failure)?,
            None => Zeroizing::new(connection.lookup_message().map_err(protocol_failure)?),
        };
        verified.validate()?;
        transport.send(&bytes).await?;
        let reply = connection
            .decode(&transport.receive().await?)
            .map_err(protocol_failure)?;
        match reply.kind {
            Some(pairing::enrollment_message::Kind::Result(result)) => {
                let receipt = ReceiptCodec::new(verified.pin)
                    .from_wire(
                        result
                            .receipt
                            .as_ref()
                            .ok_or(PairingFailure::RevocationUnconfirmed)?,
                    )
                    .map_err(|_| PairingFailure::ReceiptInvalid)?;
                Ok((receipt, connection, transport))
            }
            _ => Err(PairingFailure::AuthorizationDenied),
        }
    }
    async fn acknowledge_on(
        &self,
        work: &EnrollmentAcknowledgementWork,
        connection: &mut spaceterm_protocol::pairing_adapter::EnrollmentConnection,
        transport: &mut dyn PairingTransport,
    ) -> Result<(), PairingFailure> {
        work.validate()?;
        let binding = work.prepared.enrollment;
        let ack = pairing::EnrollmentMessage {
            kind: Some(pairing::enrollment_message::Kind::Acknowledgement(
                pairing::EnrollmentAcknowledgement {
                    operation: Some(spaceterm_protocol::pairing_adapter::operation_to_wire(
                        binding.id,
                    )),
                    request_digest: binding.digest.bytes().to_vec(),
                    acknowledged_through: 1,
                },
            )),
        };
        transport
            .send(&connection.encode(&ack).map_err(protocol_failure)?)
            .await?;
        // Serialized lookup proves the peer processed the preceding ack. A lost
        // reply leaves work retained and retries the same cumulative prefix.
        transport
            .send(&connection.lookup_message().map_err(protocol_failure)?)
            .await?;
        let reply = connection
            .decode(&transport.receive().await?)
            .map_err(protocol_failure)?;
        let Some(pairing::enrollment_message::Kind::Result(result)) = reply.kind else {
            return Err(PairingFailure::ReceiptInvalid);
        };
        let receipt = ReceiptCodec::new(work.prepared.pin)
            .from_wire(
                result
                    .receipt
                    .as_ref()
                    .ok_or(PairingFailure::ReceiptInvalid)?,
            )
            .map_err(|_| PairingFailure::ReceiptInvalid)?;
        if receipt != work.receipt {
            return Err(PairingFailure::ReceiptInvalid);
        }
        let recovered = self.store.recover().await?;
        if let Some(original) = recovered
            .enrollment_acknowledgements
            .iter()
            .find(|v| v.prepared.enrollment.id == binding.id)
        {
            if original != work {
                return Err(PairingFailure::RequestIdentityReused);
            }
            let tx = PairingJournalTransaction {
                expected_revision: recovered.revision,
                change: PairingJournalChange::FinishEnrollmentAcknowledgement {
                    operation: binding.id,
                },
            };
            require_durable(self.store.commit(&tx).await, tx.expected_revision)?;
        }
        Ok(())
    }
    /// One scheduled historical acknowledgement. It has no ordinary connection,
    /// bearer authorization, foreground prompt, enrollment submission or loop.
    pub async fn acknowledge_once(
        &self,
        operation: spaceterm_core::operations::OperationId,
        majors: Vec<u32>,
    ) -> Result<(), PairingFailure> {
        let _admission = self
            .admission
            .try_acquire()
            .map_err(|_| PairingFailure::LimitExceeded)?;
        let recovered = self.store.recover().await?;
        let work = recovered
            .enrollment_acknowledgements
            .iter()
            .find(|v| v.prepared.enrollment.id == operation)
            .ok_or(PairingFailure::InvalidRequest)?;
        work.validate()?;
        let reservation = work
            .prepared
            .reservation
            .clone()
            .ok_or(PairingFailure::InvalidDurableState)?;
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            let device_gate = CredentialGate::new(work.device.scope.clone());
            let auth_gate = CredentialGate::new(work.prepared.credential.scope.clone());
            let purpose = CredentialUsePurpose::EnrollmentAcknowledgement { operation };
            let device = self
                .credentials
                .signing_key(&work.device, &device_gate.lease(purpose))
                .await?;
            let auth = self
                .credentials
                .signing_key(&work.prepared.credential, &auth_gate.lease(purpose))
                .await?;
            let mut transport = self
                .connector
                .open(work.prepared.pin, work.prepared.route)
                .await?;
            let (client, hello) =
                EnrollmentClient::begin(work.prepared.pin, reservation, majors, device, auth)
                    .map_err(protocol_failure)?;
            transport.send(&hello).await?;
            let (ready, proof) = client
                .receive_server(&transport.receive().await?)
                .map_err(protocol_failure)?;
            transport.send(&proof).await?;
            let mut connection = ready
                .receive_ready(&transport.receive().await?)
                .map_err(protocol_failure)?;
            self.acknowledge_on(work, &mut connection, transport.as_mut())
                .await
        })
        .await
        .map_err(|_| PairingFailure::TimedOut)?
    }
}

/// Transient possession proofs for an explicit additional-Client request. The
/// server allocates the new Client identity; this record never invents one.
pub struct AdditionalClientProof {
    operation: spaceterm_core::operations::OperationId,
    command: spaceterm_core::pairing::PairingCommand,
    wire: pairing::BindAdditionalClient,
    claim_digest: spaceterm_core::operations::RequestDigest,
}
impl AdditionalClientProof {
    pub fn operation(&self) -> spaceterm_core::operations::OperationId {
        self.operation
    }
    pub fn command(&self) -> &spaceterm_core::pairing::PairingCommand {
        &self.command
    }
    pub fn wire(&self) -> &pairing::BindAdditionalClient {
        &self.wire
    }
    pub fn claim_digest(&self) -> spaceterm_core::operations::RequestDigest {
        self.claim_digest
    }
}

pub async fn prepare_additional_client_binding(
    saved: &SavedPairing,
    authority: &super::credentials::OrdinaryCredentialAuthority,
    device: &PreparedCredential,
    credentials: &dyn NativeDeviceCredentials,
    version: spaceterm_core::authority::FieldVersion,
    operation: spaceterm_core::operations::OperationId,
) -> Result<AdditionalClientProof, PairingFailure> {
    use spaceterm_core::pairing::{PairingCommand, PairingTarget, PairingTargetParts};
    use spaceterm_protocol::pairing_adapter::{
        client_binding_claim, sign_client_binding_claim, target_to_wire,
    };
    if authority.key() != saved.key()
        || operation.stream() != saved.client().parts().ordinary
        || device.public_key != saved.keys.parts().device_identity_public_key
    {
        return Err(PairingFailure::IdentityMismatch);
    }
    let device_lease = authority.device_lease(device)?;
    let auth_lease = authority.foreground_lease();
    auth_lease.validate(&saved.credential.scope)?;
    let target = PairingTarget::new(PairingTargetParts {
        server: saved.key.parts().server,
        pairing: saved.key.parts().pairing,
        device: saved.device,
    })
    .map_err(|_| PairingFailure::IdentityMismatch)?;
    let (digest, _) =
        client_binding_claim(&target, version, operation).map_err(protocol_failure)?;
    let device_signer = credentials.signing_key(device, &device_lease).await?;
    let auth_signer = credentials
        .signing_key(&saved.credential, &auth_lease)
        .await?;
    if device_signer.public_key() != device.public_key
        || auth_signer.public_key() != saved.credential.public_key
    {
        return Err(PairingFailure::CredentialIntegrity);
    }
    let (device_proof, auth_proof) = sign_client_binding_claim(
        &target,
        version,
        operation,
        device_signer.as_ref(),
        auth_signer.as_ref(),
    )
    .map_err(protocol_failure)?;
    device_lease.validate(&device.scope)?;
    auth_lease.validate(&saved.credential.scope)?;
    Ok(AdditionalClientProof {
        operation,
        wire: pairing::BindAdditionalClient {
            target: Some(target_to_wire(&target)),
            expected_authorization_version: version.get(),
            claim_digest: digest.bytes().to_vec(),
            device_identity_proof: device_proof.to_vec(),
            pairing_authentication_proof: auth_proof.to_vec(),
        },
        command: PairingCommand::BindAdditionalClient {
            target,
            expected_authorization_version: version,
            claim_digest: digest,
        },
        claim_digest: digest,
    })
}
