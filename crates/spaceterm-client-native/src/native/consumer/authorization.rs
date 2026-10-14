//! Deliberate native-owner approval of public enrollment claims. The candidate
//! keeps its credentials and journal; the producer returns a process-local,
//! single-use delivery and the existing restricted enrollment channel. The CLI's
//! retained-local-request path consumes its delivery without exposing it as output.

mod public_claim;
pub use public_claim::PublicDeviceClaimFacts;

use super::*;
use crate::{
    cli::arguments::{PublicFingerprint, PublicRequestId},
    pairing_runtime::OwnerEnrollmentRequest,
};
use serde::{Deserialize, Serialize};
use spaceterm_client::pairing::{
    credentials::{CredentialPurpose, PairingFuture, PreparedCredential},
    enrollment::{DirectEnrollmentAttempt, EnrollmentConnector, PairingTransport},
    journal::{PairingJournalSnapshot, PreparedEnrollment},
};
use spaceterm_core::{
    identity::ServerId,
    management::{ManagementProgress, ManagementProgressState, ManagementRequestReference},
    operations::{OperationBinding, OperationSequence},
    pairing::{
        EnrollmentProposal, LocalPairingGeneration, LocalPairingKeyParts, PairingOutcome,
        RevocationRouteReference,
    },
};
use spaceterm_host::{
    native_deployment::contracts::NativeRequestId,
    transport::{MessageTransport, TransportRoute},
};
use spaceterm_protocol::pairing_adapter::{
    EnrollmentAuthorization, PublicEnrollmentReservation, VerifiedDeviceClaim,
    enrollment_proposal_digest,
};
use std::time::Duration;
use zeroize::Zeroizing;

const FRAME_LIMIT: usize = 64 * 1024;
const DEADLINE: Duration = Duration::from_secs(30);

// This is the existing public owner-attempt format, not another claim store.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RetainedClaim {
    server_public_key: [u8; 32],
    proposal: EnrollmentProposal,
    request: NativeRequestId,
    generation: LocalPairingGeneration,
    device: PreparedCredential,
    authentication: PreparedCredential,
    reference: RevocationRouteReference,
}
#[derive(Serialize)]
struct OwnerRequest<'a> {
    request: NativeRequestId,
    proposal: &'a EnrollmentProposal,
    device_proof: Vec<u8>,
    authentication_proof: Vec<u8>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnerReply {
    schema: u16,
    disposition: String,
    outcome: PairingOutcome,
}

/// Verified public key-possession claim for deliberate native-owner approval.
/// Its JSON format is the production owner endpoint's `request`, `proposal`,
/// `device_proof` and `authentication_proof` record. It contains no native key
/// references, local paths or candidate journal state. Both protocol majors use
/// the same frozen `enrollment-proposal-v1` proof grammar.
pub struct PublicEnrollmentClaim {
    request: NativeRequestId,
    verified: VerifiedDeviceClaim,
    device_proof: [u8; 64],
    authentication_proof: [u8; 64],
}
impl PublicEnrollmentClaim {
    /// Verifies both candidate signatures. The request must come from the local
    /// native owner's allocator; approval binds that ID to this exact claim.
    pub fn verify(
        request: NativeRequestId,
        proposal: EnrollmentProposal,
        device_proof: &[u8],
        authentication_proof: &[u8],
    ) -> Result<Self, ManagementFailure> {
        let device_proof: [u8; 64] = device_proof
            .try_into()
            .map_err(|_| ManagementFailure::AuthorizationDenied)?;
        let authentication_proof: [u8; 64] = authentication_proof
            .try_into()
            .map_err(|_| ManagementFailure::AuthorizationDenied)?;
        let verified = VerifiedDeviceClaim::verify(proposal, &device_proof, &authentication_proof)
            .map_err(|_| ManagementFailure::AuthorizationDenied)?;
        Ok(Self {
            request,
            verified,
            device_proof,
            authentication_proof,
        })
    }
    /// Decodes the bounded production owner-request format and verifies its proofs.
    pub fn decode(bytes: &[u8]) -> Result<Self, ManagementFailure> {
        if bytes.is_empty() || bytes.len() > FRAME_LIMIT {
            return Err(ManagementFailure::LimitExceeded);
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct ClaimRecord {
            request: NativeRequestId,
            proposal: EnrollmentProposal,
            device_proof: Vec<u8>,
            authentication_proof: Vec<u8>,
        }
        let record: ClaimRecord =
            serde_json::from_slice(bytes).map_err(|_| ManagementFailure::AuthorizationDenied)?;
        Self::verify(
            record.request,
            record.proposal,
            &record.device_proof,
            &record.authentication_proof,
        )
    }
    /// Encodes public proof material only, using the production owner endpoint format.
    pub fn encode(&self) -> Result<Vec<u8>, ManagementFailure> {
        serde_json::to_vec(&OwnerRequest {
            request: self.request,
            proposal: self.proposal(),
            device_proof: self.device_proof.to_vec(),
            authentication_proof: self.authentication_proof.to_vec(),
        })
        .map_err(|_| ManagementFailure::UnsafeNativeState)
    }
    pub fn request(&self) -> NativeRequestId {
        self.request
    }
    pub fn proposal(&self) -> &EnrollmentProposal {
        self.verified.proposal()
    }
    pub fn digest(&self) -> spaceterm_core::operations::RequestDigest {
        self.verified.digest()
    }
}

/// Process-local owner delivery for a separate candidate's enrollment driver.
/// No Clone, Debug or serialization is available. The authorization zeroizes on
/// drop. Issuance replay returns public reservation data only after enrollment
/// completed; a lost single-use delivery is never replaced.
pub struct PublicEnrollmentDelivery {
    reservation: PublicEnrollmentReservation,
    authorization: Option<EnrollmentAuthorization>,
    native_request: NativeRequestId,
    native_digest: spaceterm_core::operations::RequestDigest,
    pin: ServerPin,
    detach_available: bool,
    wire_delivery: Option<Zeroizing<Vec<u8>>>,
    transport: Arc<dyn MessageTransport>,
}
impl PublicEnrollmentDelivery {
    pub fn reservation(&self) -> &PublicEnrollmentReservation {
        &self.reservation
    }
    pub fn pin(&self) -> ServerPin {
        self.pin
    }
    pub fn native_request(&self) -> NativeRequestId {
        self.native_request
    }
    pub fn native_digest(&self) -> spaceterm_core::operations::RequestDigest {
        self.native_digest
    }
    /// Moves the one delivery into the candidate's existing enrollment composition.
    /// The candidate chooses its route reference and retains all credential and
    /// journal ownership. Pass `channel` as the runtime's EnrollmentConnector,
    /// then close it after the driver completes, including on driver failure.
    pub fn into_parts(self, reference: RevocationRouteReference) -> PublicEnrollmentDeliveryParts {
        PublicEnrollmentDeliveryParts {
            reservation: self.reservation,
            authorization: self.authorization,
            native_request: self.native_request,
            native_digest: self.native_digest,
            channel: Arc::new(PublicEnrollmentChannel {
                connector: AuthorizationConnector {
                    pin: self.pin,
                    reference,
                    transport: Mutex::new(Some(self.transport.clone())),
                },
                transport: self.transport,
            }),
        }
    }
    /// Moves the original frozen delivery to the private-pipe CLI bridge and
    /// closes the producer channel. The caller can consume it over the server's
    /// public enrollment endpoint with its own native connector and journal.
    /// Historical receipt-only replay is not another authorization delivery.
    pub async fn into_protocol_delivery(
        self,
    ) -> Result<
        crate::cli::bindings::PairingAuthorizationDelivery,
        crate::cli::bindings::PairingAuthorizationFailure,
    > {
        use crate::cli::bindings::{PairingAuthorizationDelivery, PairingAuthorizationFailure};
        let request = ManagementRequestReference::Native(*self.native_request.as_bytes());
        close(self.transport.as_ref())
            .await
            .map_err(|failure| PairingAuthorizationFailure::new(failure, Some(request)))?;
        if !self.detach_available {
            return Err(PairingAuthorizationFailure::new(
                ManagementFailure::AuthorizationUnavailable,
                Some(request),
            ));
        }
        let bytes = self.wire_delivery.ok_or_else(|| {
            PairingAuthorizationFailure::new(ManagementFailure::OutcomePending, Some(request))
        })?;
        PairingAuthorizationDelivery::new(request, bytes)
    }
    /// Explicitly joins the native owner channel when the caller declines enrollment.
    pub async fn close(self) -> Result<(), ManagementFailure> {
        close(self.transport.as_ref()).await
    }
}

/// Consumed delivery parts. The bearer owner is zeroizing and cannot be cloned.
/// These values belong in memory, never in CLI output or a retained journal.
pub struct PublicEnrollmentDeliveryParts {
    pub reservation: PublicEnrollmentReservation,
    pub authorization: Option<EnrollmentAuthorization>,
    pub native_request: NativeRequestId,
    pub native_digest: spaceterm_core::operations::RequestDigest,
    pub channel: Arc<PublicEnrollmentChannel>,
}

/// One restricted enrollment session on the production native owner provider.
/// Opening takes the channel once and checks the exact pin and caller-selected
/// route reference. It grants no ordinary Client authentication or local paths.
pub struct PublicEnrollmentChannel {
    connector: AuthorizationConnector,
    transport: Arc<dyn MessageTransport>,
}
impl PublicEnrollmentChannel {
    pub async fn close(&self) -> Result<(), ManagementFailure> {
        self.connector
            .transport
            .lock()
            .map_err(|_| ManagementFailure::UnsafeNativeState)?
            .take();
        close(self.transport.as_ref()).await
    }
}
impl EnrollmentConnector for PublicEnrollmentChannel {
    fn open<'a>(
        &'a self,
        pin: ServerPin,
        reference: RevocationRouteReference,
    ) -> PairingFuture<'a, Result<Box<dyn PairingTransport>, PairingFailure>> {
        self.connector.open(pin, reference)
    }
}

impl NativeConsumer {
    /// Approves a verified public claim with an explicit candidate fingerprint.
    /// This deliberate native-owner producer uses the account/build-selected
    /// running owner endpoint or signed stopped-owner command. It does not load
    /// candidate credentials, borrow a CLI journal, prepare a device, recover the
    /// account or start a service. The candidate consumes the returned delivery
    /// through its own public NativePairingRuntime enrollment driver.
    pub async fn authorize_public_claim(
        &self,
        claim: &PublicEnrollmentClaim,
        expected_device_fingerprint: [u8; 32],
    ) -> Result<PublicEnrollmentDelivery, ManagementFailure> {
        self.produce_public_claim(claim, expected_device_fingerprint, false, None)
            .await
    }

    async fn produce_public_claim(
        &self,
        claim: &PublicEnrollmentClaim,
        expected_device_fingerprint: [u8; 32],
        detached: bool,
        admission: Option<&crate::cli::bindings::PairingAuthorizationAdmission>,
    ) -> Result<PublicEnrollmentDelivery, ManagementFailure> {
        self.mutable()?;
        if claim
            .proposal()
            .parts()
            .keys
            .parts()
            .device_identity_public_key
            != expected_device_fingerprint
        {
            return Err(ManagementFailure::AuthorizationDenied);
        }
        let _mutation = self
            .mutation
            .try_lock()
            .map_err(|_| ManagementFailure::OutcomePending)?;
        let registration = self
            .native
            .supervisor
            .discover()
            .map_err(classify_service)?
            .ok_or(ManagementFailure::NativeUnavailable)?;
        if !registration.is_installed() {
            return Err(ManagementFailure::NativeUnavailable);
        }
        // A detached delivery needs the running server's public enrollment
        // listener. Closing a stopped-owner subprocess ends its only executor.
        if detached
            && registration.desired_state() != spaceterm_host::startup::DesiredServerState::Running
        {
            return Err(ManagementFailure::AuthorizationUnavailable);
        }
        self.require_account_recovery_complete()?;
        let server = ServerId::new(registration.server_pin().identity);
        if claim.proposal().parts().server != server {
            return Err(ManagementFailure::AuthorizationDenied);
        }
        self.deliver_public_claim(claim, &registration, admission)
            .await
    }

    async fn deliver_public_claim(
        &self,
        claim: &PublicEnrollmentClaim,
        registration: &RegisteredServer,
        admission: Option<&crate::cli::bindings::PairingAuthorizationAdmission>,
    ) -> Result<PublicEnrollmentDelivery, ManagementFailure> {
        let server = ServerId::new(registration.server_pin().identity);
        let supervisor = &self.native.supervisor;
        let native_digest = match supervisor
            .retained_owner_enrollment_digest(claim.request(), server, claim.digest())
            .map_err(classify_service)?
        {
            Some(original) => original,
            None => {
                let digest = supervisor
                    .owner_enrollment_digest(claim.request(), server, claim.digest())
                    .map_err(classify_service)?;
                if let Some(admission) = admission {
                    admission.record(ManagementRequestReference::Native(
                        *claim.request().as_bytes(),
                    ));
                }
                supervisor
                    .record_owner_enrollment(claim.request(), digest, server, claim.digest())
                    .map_err(classify_service)?;
                digest
            }
        };
        if let Some(admission) = admission {
            admission.record(ManagementRequestReference::Native(
                *claim.request().as_bytes(),
            ));
        }
        let transport = self
            .owner_channels
            .open(registration)
            .await
            .map_err(classify_client)?;
        let result = tokio::time::timeout(DEADLINE, async {
            let peer = transport.peer_facts();
            if peer.route != TransportRoute::PrivateIpc
                || peer.local_uid != Some(registration.key().account_uid)
            {
                return Err(ManagementFailure::AuthorizationDenied);
            }
            transport
                .send(&claim.encode()?)
                .await
                .map_err(|_| ManagementFailure::Offline)?;
            let reply: OwnerReply = serde_json::from_slice(&receive(transport.as_ref()).await?)
                .map_err(|_| ManagementFailure::UnsafeNativeState)?;
            let (ticket, enrollment) = match reply.outcome {
                PairingOutcome::AuthorizationReserved {
                    ticket,
                    enrollment,
                    claim_digest,
                } if reply.schema == 1 && claim_digest == claim.digest() => (ticket, enrollment),
                _ => return Err(ManagementFailure::AuthorizationDenied),
            };
            let bytes = receive(transport.as_ref()).await?;
            let (reservation, authorization, wire_delivery) = match reply.disposition.as_str() {
                "authorization_delivered" => {
                    let (reservation, authorization) =
                        EnrollmentAuthorization::from_delivery(Zeroizing::new(bytes.to_vec()))
                            .map_err(|_| ManagementFailure::UnsafeNativeState)?;
                    (reservation, Some(authorization), Some(bytes))
                }
                "receipt_available" => (
                    PublicEnrollmentReservation::decode(&bytes)
                        .map_err(|_| ManagementFailure::UnsafeNativeState)?,
                    None,
                    None,
                ),
                "authorization_reply_lost" | "authorization_invalidated" => {
                    return Err(ManagementFailure::OutcomePending);
                }
                _ => return Err(ManagementFailure::AuthorizationDenied),
            };
            let parts = reservation.parts();
            if parts.ticket != ticket
                || parts.claim_digest != claim.digest()
                || parts.keys != claim.proposal().parts().keys
                || parts.target.parts().server != server
                || parts.target.parts().device != claim.proposal().parts().device
                || OperationId::new(
                    parts.enrollment_stream,
                    OperationSequence::new(1).map_err(|_| ManagementFailure::UnsafeNativeState)?,
                ) != enrollment
            {
                return Err(ManagementFailure::AuthorizationDenied);
            }
            Ok(PublicEnrollmentDelivery {
                reservation,
                authorization,
                native_request: claim.request(),
                native_digest,
                pin: registration.server_pin(),
                detach_available: registration.desired_state()
                    == spaceterm_host::startup::DesiredServerState::Running,
                wire_delivery,
                transport: transport.clone(),
            })
        })
        .await
        .map_err(|_| ManagementFailure::Offline)
        .and_then(|result| result);
        if result.is_err() {
            let _ = close(transport.as_ref()).await;
        }
        result
    }

    pub(super) async fn authorize_public_claim_json(
        &self,
        bytes: Zeroizing<Vec<u8>>,
        expected: PublicFingerprint,
    ) -> Result<
        crate::cli::bindings::PairingAuthorizationDelivery,
        crate::cli::bindings::PairingAuthorizationFailure,
    > {
        self.authorize_public_claim_json_admitted(
            bytes,
            expected,
            crate::cli::bindings::PairingAuthorizationAdmission::default(),
        )
        .await
    }

    pub(super) async fn authorize_public_claim_json_admitted(
        &self,
        bytes: Zeroizing<Vec<u8>>,
        expected: PublicFingerprint,
        admission: crate::cli::bindings::PairingAuthorizationAdmission,
    ) -> Result<
        crate::cli::bindings::PairingAuthorizationDelivery,
        crate::cli::bindings::PairingAuthorizationFailure,
    > {
        use crate::cli::bindings::PairingAuthorizationFailure;
        match public_claim::decode(&bytes)? {
            public_claim::ClaimInput::Device(claim) => {
                self.authorize_device_claim(claim, expected, admission)
                    .await
            }
            public_claim::ClaimInput::Legacy(claim) => {
                let request = ManagementRequestReference::Native(*claim.request().as_bytes());
                self.produce_public_claim(&claim, expected.bytes(), true, Some(&admission))
                    .await
                    .map_err(|failure| PairingAuthorizationFailure::new(failure, Some(request)))?
                    .into_protocol_delivery()
                    .await
            }
        }
    }

    pub(super) async fn authorize_registered(
        &self,
        request: PublicRequestId,
        expected: PublicFingerprint,
    ) -> Result<ManagementProgress, ManagementFailure> {
        self.mutable()?;
        let _mutation = self
            .mutation
            .try_lock()
            .map_err(|_| ManagementFailure::OutcomePending)?;
        let registration = self
            .native
            .supervisor
            .discover()
            .map_err(classify_service)?
            .ok_or(ManagementFailure::NativeUnavailable)?;
        if !registration.is_installed() {
            return Err(ManagementFailure::NativeUnavailable);
        }
        // Approval is not enrollment preparation. An absent claim must not create
        // consumer storage, allocate a request, or acquire a new credential.
        let directory = self.native.supervisor.client_connections_directory();
        match std::fs::symlink_metadata(&directory) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(ManagementFailure::AuthorizationDenied);
            }
            Err(_) => return Err(ManagementFailure::StorageUnavailable),
            Ok(_) => {}
        }
        let admission = self
            .native
            .supervisor
            .admit_client_provisioning()
            .map_err(classify_service)?;
        self.require_account_recovery_complete()?;
        let bytes = admission
            .owner_attempt("enrollment")
            .map_err(classify_service)?
            .ok_or(ManagementFailure::AuthorizationDenied)?;
        let claim: RetainedClaim =
            serde_json::from_slice(&bytes).map_err(|_| ManagementFailure::UnsafeNativeState)?;
        let pin = registration.server_pin();
        let server = ServerId::new(pin.identity);
        if claim.request.as_bytes() != &request.bytes()
            || expected.bytes()
                != claim
                    .proposal
                    .parts()
                    .keys
                    .parts()
                    .device_identity_public_key
            || claim.server_public_key != pin.public_key
            || claim.proposal.parts().server != server
            || claim
                .proposal
                .parts()
                .keys
                .parts()
                .device_identity_public_key
                != claim.device.public_key
            || claim
                .proposal
                .parts()
                .keys
                .parts()
                .pairing_authentication_public_key
                != claim.authentication.public_key
            || claim.device.scope.parts().purpose != CredentialPurpose::DeviceIdentity
            || claim.authentication.scope.parts().purpose
                != (CredentialPurpose::PairingAuthentication { server })
        {
            return Err(ManagementFailure::AuthorizationDenied);
        }
        let codec = Arc::new(CanonicalCodec);
        let verifier = Arc::new(PinnedReceipts);
        let limits = JournalLimits::default();
        let storage = NativeClientJournal::open_existing(
            directory.join("enrollment"),
            codec.clone(),
            verifier.clone(),
            limits,
        )
        .await
        .map_err(classify_client)?;
        let journal = Arc::new(
            JournalController::recover(Arc::new(storage), codec, verifier, limits)
                .await
                .map_err(classify_client)?,
        );
        let local = LocalRouteAuthority::new(registration.endpoint().map_err(classify_service)?)
            .map_err(classify_client)?;
        let route = local.description().clone();
        let restricted = Arc::new(NativePairingConnector::new(
            journal.clone(),
            NativeRouteConnector::new(vec![local], None).map_err(classify_client)?,
        ));
        let pairing = NativePairingRuntime::with_adapters(
            journal.clone(),
            self.credentials.clone(),
            restricted.clone(),
            restricted.clone(),
            Arc::new(ConsumerInvalidator::default()),
            self.owner_handshake.clone(),
        );
        validate_retained(
            &claim,
            &pairing.inventory().await.map_err(classify_pairing)?,
        )?;
        let attempt = DirectEnrollmentAttempt::new(pin, claim.reference, claim.generation.get())
            .map_err(classify_pairing)?;
        let verified = attempt
            .confirm(
                pin,
                claim.reference,
                claim.generation.get(),
                &attempt.fingerprint(),
            )
            .map_err(classify_pairing)?;
        let device_key = self
            .credentials
            .signing_key(
                &claim.device,
                &verified
                    .credential_use(&claim.device)
                    .map_err(classify_pairing)?,
            )
            .await
            .map_err(classify_pairing)?;
        let authentication_key = self
            .credentials
            .signing_key(
                &claim.authentication,
                &verified
                    .credential_use(&claim.authentication)
                    .map_err(classify_pairing)?,
            )
            .await
            .map_err(classify_pairing)?;
        let (device_proof, authentication_proof) = VerifiedDeviceClaim::proofs(
            &claim.proposal,
            device_key.as_ref(),
            authentication_key.as_ref(),
        )
        .map_err(|_| ManagementFailure::CredentialUnavailable)?;
        let digest = enrollment_proposal_digest(&claim.proposal)
            .map_err(|_| ManagementFailure::UnsafeNativeState)?;
        let supervisor = &self.native.supervisor;
        let native_digest = match supervisor
            .retained_owner_enrollment_digest(claim.request, server, digest)
            .map_err(classify_service)?
        {
            Some(original) => original,
            None => {
                let original = supervisor
                    .owner_enrollment_digest(claim.request, server, digest)
                    .map_err(classify_service)?;
                supervisor
                    .record_owner_enrollment(claim.request, original, server, digest)
                    .map_err(classify_service)?;
                original
            }
        };
        let transport = self
            .owner_channels
            .open(&registration)
            .await
            .map_err(classify_client)?;
        let result = tokio::time::timeout(DEADLINE, async {
            let peer = transport.peer_facts();
            if peer.route != TransportRoute::PrivateIpc
                || peer.local_uid != Some(registration.key().account_uid)
            {
                return Err(ManagementFailure::AuthorizationDenied);
            }
            let bytes = serde_json::to_vec(&OwnerRequest {
                request: claim.request,
                proposal: &claim.proposal,
                device_proof: device_proof.to_vec(),
                authentication_proof: authentication_proof.to_vec(),
            })
            .map_err(|_| ManagementFailure::UnsafeNativeState)?;
            transport
                .send(&bytes)
                .await
                .map_err(|_| ManagementFailure::Offline)?;
            let reply: OwnerReply = serde_json::from_slice(&receive(transport.as_ref()).await?)
                .map_err(|_| ManagementFailure::UnsafeNativeState)?;
            let (ticket, enrollment) = match reply.outcome {
                PairingOutcome::AuthorizationReserved {
                    ticket,
                    enrollment,
                    claim_digest,
                } if reply.schema == 1 && claim_digest == digest => (ticket, enrollment),
                _ => return Err(ManagementFailure::AuthorizationDenied),
            };
            let delivery = receive(transport.as_ref()).await?;
            let (reservation, authorization) = match reply.disposition.as_str() {
                "authorization_delivered" => {
                    let (reservation, authorization) =
                        EnrollmentAuthorization::from_delivery(delivery)
                            .map_err(|_| ManagementFailure::UnsafeNativeState)?;
                    (reservation, Some(authorization))
                }
                "receipt_available" => (
                    PublicEnrollmentReservation::decode(&delivery)
                        .map_err(|_| ManagementFailure::UnsafeNativeState)?,
                    None,
                ),
                "authorization_reply_lost" | "authorization_invalidated" => {
                    return Err(ManagementFailure::OutcomePending);
                }
                _ => return Err(ManagementFailure::AuthorizationDenied),
            };
            let parts = reservation.parts();
            if parts.ticket != ticket
                || parts.claim_digest != digest
                || parts.keys != claim.proposal.parts().keys
                || parts.target.parts().server != server
                || parts.target.parts().device != claim.proposal.parts().device
                || OperationId::new(
                    parts.enrollment_stream,
                    OperationSequence::new(1).map_err(|_| ManagementFailure::UnsafeNativeState)?,
                ) != enrollment
            {
                return Err(ManagementFailure::AuthorizationDenied);
            }
            let prepared = PreparedEnrollment {
                local_pairing: LocalPairingKey::new(LocalPairingKeyParts {
                    server,
                    pairing: parts.target.parts().pairing,
                    generation: claim.generation,
                })
                .map_err(|_| ManagementFailure::UnsafeNativeState)?,
                device: parts.target.parts().device,
                pin,
                keys: parts.keys.clone(),
                client: parts.initial_client,
                credential: claim.authentication.clone(),
                route: claim.reference,
                enrollment: OperationBinding {
                    id: enrollment,
                    digest: parts.enrollment_request_digest,
                },
                reservation: Some(reservation),
            };
            let owner = NativePairingRuntime::with_adapters(
                journal,
                self.credentials.clone(),
                Arc::new(AuthorizationConnector {
                    pin,
                    reference: claim.reference,
                    transport: Mutex::new(Some(transport.clone())),
                }),
                restricted,
                Arc::new(ConsumerInvalidator::default()),
                self.owner_handshake.clone(),
            );
            if owner
                .inventory()
                .await
                .map_err(classify_pairing)?
                .enrollment_acknowledgements
                .iter()
                .any(|ack| ack.prepared.enrollment.id == prepared.enrollment.id)
            {
                owner
                    .acknowledge_enrollment_once(prepared.enrollment.id)
                    .await
                    .map_err(classify_pairing)?;
            }
            owner
                .enroll_account_owner(
                    supervisor,
                    OwnerEnrollmentRequest {
                        prepared: &prepared,
                        device: &claim.device,
                        authorization: authorization.as_ref(),
                        native_request: claim.request,
                        native_digest,
                    },
                    route,
                )
                .await
                .map_err(classify_pairing)?;
            Ok(ManagementProgress {
                request: ManagementRequestReference::Native(*claim.request.as_bytes()),
                state: ManagementProgressState::Completed,
                completed_units: 1,
                total_units: 1,
            })
        })
        .await
        .map_err(|_| ManagementFailure::Offline)
        .and_then(|result| result);
        let closed = transport
            .close()
            .await
            .map_err(|_| ManagementFailure::Offline);
        let progress = result?;
        closed?;
        Ok(progress)
    }
}

fn validate_retained(
    claim: &RetainedClaim,
    inventory: &PairingJournalSnapshot,
) -> Result<(), ManagementFailure> {
    let device = inventory
        .credential_preparations
        .iter()
        .any(|prepared| prepared.credential.as_ref() == Some(&claim.device));
    let authentication = inventory.credential_preparations.iter().any(|prepared| {
        prepared.generation == claim.generation
            && prepared.credential.as_ref() == Some(&claim.authentication)
    }) || inventory.prepared.iter().any(|prepared| {
        prepared.local_pairing.parts().generation == claim.generation
            && prepared.credential == claim.authentication
    }) || inventory.enrollment_acknowledgements.iter().any(|ack| {
        ack.prepared.local_pairing.parts().generation == claim.generation
            && ack.prepared.credential == claim.authentication
    }) || inventory.saved.iter().any(|saved| {
        saved.key().parts().generation == claim.generation
            && saved.credential() == &claim.authentication
    });
    if !device || !authentication {
        return Err(ManagementFailure::AuthorizationDenied);
    }
    // The same request cannot silently restore a locally removed incarnation.
    if inventory
        .suppressed
        .iter()
        .any(|key| key.parts().generation == claim.generation)
        || inventory.revocations.iter().any(|pending| {
            pending.task.parts().local_pairing.parts().generation == claim.generation
        })
    {
        return Err(ManagementFailure::Revoked);
    }
    Ok(())
}
async fn close(transport: &dyn MessageTransport) -> Result<(), ManagementFailure> {
    tokio::time::timeout(DEADLINE, transport.close())
        .await
        .map_err(|_| ManagementFailure::Offline)?
        .map_err(|_| ManagementFailure::Offline)
}
async fn receive(
    transport: &dyn MessageTransport,
) -> Result<Zeroizing<Vec<u8>>, ManagementFailure> {
    let bytes = Zeroizing::new(
        transport
            .recv()
            .await
            .map_err(|_| ManagementFailure::Offline)?
            .ok_or(ManagementFailure::Offline)?,
    );
    if bytes.is_empty() || bytes.len() > FRAME_LIMIT {
        return Err(ManagementFailure::Offline);
    }
    Ok(bytes)
}
struct AuthorizationConnector {
    pin: ServerPin,
    reference: RevocationRouteReference,
    transport: Mutex<Option<Arc<dyn MessageTransport>>>,
}
impl EnrollmentConnector for AuthorizationConnector {
    fn open<'a>(
        &'a self,
        pin: ServerPin,
        reference: RevocationRouteReference,
    ) -> PairingFuture<'a, Result<Box<dyn PairingTransport>, PairingFailure>> {
        Box::pin(async move {
            if pin != self.pin || reference != self.reference {
                return Err(PairingFailure::IdentityMismatch);
            }
            let transport = self
                .transport
                .lock()
                .map_err(|_| PairingFailure::Offline)?
                .take()
                .ok_or(PairingFailure::Offline)?;
            Ok(Box::new(AuthorizationTransport(transport)) as Box<dyn PairingTransport>)
        })
    }
}
struct AuthorizationTransport(Arc<dyn MessageTransport>);
impl PairingTransport for AuthorizationTransport {
    fn send<'a>(&'a mut self, bytes: &'a [u8]) -> PairingFuture<'a, Result<(), PairingFailure>> {
        Box::pin(async move {
            self.0
                .send(bytes)
                .await
                .map_err(|_| PairingFailure::Offline)
        })
    }
    fn receive(&mut self) -> PairingFuture<'_, Result<Zeroizing<Vec<u8>>, PairingFailure>> {
        Box::pin(async move {
            receive(self.0.as_ref())
                .await
                .map_err(|_| PairingFailure::Offline)
        })
    }
}
