//! Request-free public claims acquire local authority only after verification.
use super::*;
use crate::cli::bindings::{PairingAuthorizationDelivery, PairingAuthorizationFailure};
use spaceterm_core::operations::RequestDigest;
use spaceterm_host::account_service::{ClientProvisioning, ServiceSupervisor};

const HISTORY_LIMIT: usize = 64;
const METADATA: &str = "public-claims";

// This is exactly the PublicDeviceClaim JSON exported by prepare_public_claim.
// The legacy owner endpoint record remains a separate, frozen input format.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeviceClaimRecord {
    schema: u16,
    server_identity: [u8; 16],
    server_public_key: [u8; 32],
    proposal: EnrollmentProposal,
    device_proof: Vec<u8>,
    authentication_proof: Vec<u8>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyClaimRecord {
    request: NativeRequestId,
    proposal: EnrollmentProposal,
    device_proof: Vec<u8>,
    authentication_proof: Vec<u8>,
}
#[derive(Deserialize)]
#[serde(untagged)]
enum ClaimRecord {
    Device(DeviceClaimRecord),
    Legacy(LegacyClaimRecord),
}

pub(super) enum ClaimInput {
    Device(VerifiedPublicClaim),
    Legacy(PublicEnrollmentClaim),
}
pub(super) struct VerifiedPublicClaim {
    pin: ServerPin,
    verified: VerifiedDeviceClaim,
    device_proof: [u8; 64],
    authentication_proof: [u8; 64],
}

pub(super) fn decode(bytes: &[u8]) -> Result<ClaimInput, ManagementFailure> {
    if bytes.is_empty() || bytes.len() > FRAME_LIMIT {
        return Err(ManagementFailure::LimitExceeded);
    }
    let record: ClaimRecord =
        serde_json::from_slice(bytes).map_err(|_| ManagementFailure::AuthorizationDenied)?;
    match record {
        ClaimRecord::Legacy(record) => Ok(ClaimInput::Legacy(PublicEnrollmentClaim::verify(
            record.request,
            record.proposal,
            &record.device_proof,
            &record.authentication_proof,
        )?)),
        ClaimRecord::Device(record) => {
            if record.schema != 1
                || record.server_identity == [0; 16]
                || record.server_public_key == [0; 32]
                || record.proposal.parts().server.bytes() != record.server_identity
            {
                return Err(ManagementFailure::AuthorizationDenied);
            }
            let device_proof: [u8; 64] = record
                .device_proof
                .as_slice()
                .try_into()
                .map_err(|_| ManagementFailure::AuthorizationDenied)?;
            let authentication_proof: [u8; 64] = record
                .authentication_proof
                .as_slice()
                .try_into()
                .map_err(|_| ManagementFailure::AuthorizationDenied)?;
            let verified =
                VerifiedDeviceClaim::verify(record.proposal, &device_proof, &authentication_proof)
                    .map_err(|_| ManagementFailure::AuthorizationDenied)?;
            Ok(ClaimInput::Device(VerifiedPublicClaim {
                pin: ServerPin {
                    identity: record.server_identity,
                    public_key: record.server_public_key,
                },
                verified,
                device_proof,
                authentication_proof,
            }))
        }
    }
}

/// Public facts of one exported device claim, read after both key proofs verify.
/// An owner compares `device_fingerprint` with the fingerprint the claimant shows
/// before authorizing. Reading grants no authority and touches no native state.
pub struct PublicDeviceClaimFacts {
    server: ServerPin,
    device: PublicFingerprint,
}
impl PublicDeviceClaimFacts {
    /// Accepts only the schema 1 PublicDeviceClaim document, not the legacy
    /// owner-request record, because only it names the server's full pin.
    pub fn inspect(bytes: &[u8]) -> Result<Self, ManagementFailure> {
        match decode(bytes)? {
            ClaimInput::Device(claim) => Ok(Self {
                server: claim.pin,
                device: PublicFingerprint::from_public_key(
                    claim
                        .verified
                        .proposal()
                        .parts()
                        .keys
                        .parts()
                        .device_identity_public_key,
                ),
            }),
            ClaimInput::Legacy(_) => Err(ManagementFailure::AuthorizationDenied),
        }
    }
    pub fn server(&self) -> ServerPin {
        self.server
    }
    /// The claimant's device identity public key, the authorization's expected fingerprint.
    pub fn device_fingerprint(&self) -> PublicFingerprint {
        self.device
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClaimBindings {
    schema: u16,
    bindings: Vec<ClaimBinding>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClaimBinding {
    claim_digest: RequestDigest,
    server_identity: [u8; 16],
    server_public_key: [u8; 32],
    request: NativeRequestId,
}
impl ClaimBindings {
    fn validate(&self) -> Result<(), ManagementFailure> {
        if self.schema != 1 || self.bindings.len() > HISTORY_LIMIT {
            return Err(ManagementFailure::UnsafeNativeState);
        }
        for (index, binding) in self.bindings.iter().enumerate() {
            if binding.claim_digest.bytes() == [0; 32]
                || binding.server_identity == [0; 16]
                || binding.server_public_key == [0; 32]
                || self.bindings[..index].iter().any(|prior| {
                    prior.claim_digest == binding.claim_digest || prior.request == binding.request
                })
            {
                return Err(ManagementFailure::UnsafeNativeState);
            }
        }
        Ok(())
    }

    fn bind(
        admission: &ClientProvisioning,
        supervisor: &ServiceSupervisor,
        claim: &VerifiedPublicClaim,
        observer: &crate::cli::bindings::PairingAuthorizationAdmission,
    ) -> Result<NativeRequestId, PairingAuthorizationFailure> {
        let previous = admission
            .owner_attempt(METADATA)
            .map_err(classify_service)?;
        let mut retained: Self = match &previous {
            Some(bytes) => {
                serde_json::from_slice(bytes).map_err(|_| ManagementFailure::UnsafeNativeState)?
            }
            None => Self {
                schema: 1,
                bindings: Vec::new(),
            },
        };
        retained.validate()?;
        let digest = claim.verified.digest();
        if let Some(original) = retained
            .bindings
            .iter()
            .find(|binding| binding.claim_digest == digest)
        {
            let request = ManagementRequestReference::Native(*original.request.as_bytes());
            if original.server_identity != claim.pin.identity
                || original.server_public_key != claim.pin.public_key
            {
                return Err(PairingAuthorizationFailure::new(
                    ManagementFailure::AuthorizationDenied,
                    Some(request),
                ));
            }
            observer.record(request);
            // Delivery validates this provider-issued request, including retired
            // archives. A missing or conflicting host binding is never reallocated.
            return Ok(original.request);
        }
        if retained.bindings.len() == HISTORY_LIMIT {
            // No eviction: forgetting an old digest could replace its lost bearer.
            return Err(ManagementFailure::LimitExceeded.into());
        }
        let request = supervisor.allocate_request().map_err(classify_service)?;
        let reference = ManagementRequestReference::Native(*request.as_bytes());
        observer.record(reference);
        retained.bindings.push(ClaimBinding {
            claim_digest: digest,
            server_identity: claim.pin.identity,
            server_public_key: claim.pin.public_key,
            request,
        });
        let bytes = serde_json::to_vec(&retained).map_err(|_| {
            PairingAuthorizationFailure::new(ManagementFailure::UnsafeNativeState, Some(reference))
        })?;
        admission
            .retain_owner_attempt(METADATA, previous.as_deref(), &bytes)
            .map_err(|failure| {
                PairingAuthorizationFailure::new(classify_service(failure), Some(reference))
            })?;
        Ok(request)
    }
}

impl NativeConsumer {
    pub(super) async fn authorize_device_claim(
        &self,
        claim: VerifiedPublicClaim,
        expected: PublicFingerprint,
        observer: crate::cli::bindings::PairingAuthorizationAdmission,
    ) -> Result<PairingAuthorizationDelivery, PairingAuthorizationFailure> {
        self.mutable()?;
        if claim
            .verified
            .proposal()
            .parts()
            .keys
            .parts()
            .device_identity_public_key
            != expected.bytes()
        {
            return Err(ManagementFailure::AuthorizationDenied.into());
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
            return Err(ManagementFailure::NativeUnavailable.into());
        }
        if registration.server_pin() != claim.pin {
            return Err(ManagementFailure::AuthorizationDenied.into());
        }
        if registration.desired_state() != spaceterm_host::startup::DesiredServerState::Running {
            return Err(ManagementFailure::AuthorizationUnavailable.into());
        }
        // All public input checks precede storage admission and request allocation.
        // Admission first recovers an exact interrupted metadata publication. Read
        // recovery intent under that lease so its shared-directory fence cannot
        // prevent this deliberate retry from recovering its original binding.
        // Every original binding remains retained, including lost delivery attempts.
        let admission = self
            .native
            .supervisor
            .admit_client_provisioning()
            .map_err(classify_service)?;
        self.require_account_recovery_complete()?;
        let request = ClaimBindings::bind(&admission, &self.native.supervisor, &claim, &observer)?;
        let reference = ManagementRequestReference::Native(*request.as_bytes());
        let bound = PublicEnrollmentClaim {
            request,
            verified: claim.verified,
            device_proof: claim.device_proof,
            authentication_proof: claim.authentication_proof,
        };
        let delivery = self
            .deliver_public_claim(&bound, &registration, Some(&observer))
            .await
            .map_err(|failure| PairingAuthorizationFailure::new(failure, Some(reference)))?;
        drop(admission);
        delivery.into_protocol_delivery().await
    }
}
