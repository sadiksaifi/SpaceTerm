//! Deliberate account-owner issuance is a separate native boundary. No peer UID,
//! ordinary Principal, or fabricated pairing-bound operation supplies this authority.

use spaceterm_core::identity::ServerId;
use spaceterm_core::operations::RequestDigest;
use spaceterm_core::pairing::{EnrollmentIssuer, EnrollmentProposal, PairingFailure};
use spaceterm_host::native_deployment::contracts::NativeRequestId;

/// Implemented by the trusted native request journal/provider. It verifies the
/// deliberate request's durable identity, owning account/build registration,
/// original Server, claim and current OS authorization inside actor serialization.
/// It must preserve deliberate Stopped state; issuance never invokes Start.
pub trait AccountOwnerEnrollmentPort: Send + Sync {
    /// Rechecks the native journal's original claim/authorization at activation.
    /// Historical completed lookup must not call this or require renewed authority.
    fn revalidate_owner_issuance(
        &self,
        request: &NativeRequestId,
        digest: RequestDigest,
        server: ServerId,
        claim_digest: RequestDigest,
    ) -> Result<(), PairingFailure>;
    fn validate_owner_request(
        &self,
        request: &NativeRequestId,
        digest: RequestDigest,
        server: ServerId,
        proposal: &EnrollmentProposal,
    ) -> Result<(), PairingFailure>;
}

/// Opaque short-lived issuance capability. Only validation through the injected
/// native boundary constructs it. It is not deserializable or caller-supplied.
pub struct EnrollmentAuthority {
    issuer: EnrollmentIssuer,
}
impl EnrollmentAuthority {
    pub(crate) fn owner(
        port: &dyn AccountOwnerEnrollmentPort,
        request: &NativeRequestId,
        digest: RequestDigest,
        server: ServerId,
        proposal: &EnrollmentProposal,
    ) -> Result<Self, PairingFailure> {
        port.validate_owner_request(request, digest, server, proposal)?;
        Ok(Self {
            issuer: EnrollmentIssuer::AccountOwner {
                native_request: *request.as_bytes(),
                native_request_digest: digest,
            },
        })
    }
    pub(crate) fn paired(
        operation: spaceterm_core::operations::OperationBinding,
        version: spaceterm_core::authority::FieldVersion,
    ) -> Self {
        Self {
            issuer: EnrollmentIssuer::Paired {
                operation,
                authorization_version: version,
            },
        }
    }
    pub(crate) fn into_issuer(self) -> EnrollmentIssuer {
        self.issuer
    }
}
