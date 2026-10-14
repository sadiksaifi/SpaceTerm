//! Foreground public device claims reuse native key custody and the canonical journal.

use super::*;
use ring::rand::{SecureRandom as _, SystemRandom};
use spaceterm_client::pairing::{
    credentials::{CredentialNamespace, CredentialPurpose},
    enrollment::{VerifiedServer, fingerprint},
};
use spaceterm_core::{
    identity::{DeviceId, ServerId},
    pairing::{EnrollmentProposal, EnrollmentProposalParts, PairingKeys, PairingKeysParts},
};
use spaceterm_protocol::pairing_adapter::VerifiedDeviceClaim;

/// Public proof of native key possession. This contains no enrollment bearer,
/// native locator, credential reference or private key material.
#[derive(serde::Serialize)]
pub struct PublicDeviceClaim {
    pub schema: u16,
    pub server_identity: [u8; 16],
    pub server_public_key: [u8; 32],
    pub proposal: EnrollmentProposal,
    pub device_proof: Vec<u8>,
    pub authentication_proof: Vec<u8>,
}

static PREPARATION: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

impl NativePairingRuntime {
    /// A deliberate claim export requires an out-of-band server fingerprint
    /// comparison before creating or using native keys. It never enrolls,
    /// installs, starts, or opens a connection.
    pub async fn prepare_public_claim(
        &self,
        namespace: CredentialNamespace,
        pin: ServerPin,
        route: RouteDescription,
        compared_fingerprint: &str,
    ) -> Result<PublicDeviceClaim, PairingFailure> {
        if pin.identity == [0; 16] || pin.public_key == [0; 32] {
            return Err(PairingFailure::InvalidRequest);
        }
        if compared_fingerprint != fingerprint(pin.public_key) {
            return Err(PairingFailure::FingerprintMismatch);
        }
        let _preparation = PREPARATION.lock().await;
        self.journal.ensure_healthy().map_err(classify)?;
        let server = ServerId::new(pin.identity);
        let inventory = self.inventory().await?;
        if inventory.saved.iter().any(|saved| {
            saved.key().parts().server == server && !inventory.is_suppressed(saved.key())
        }) {
            return Err(PairingFailure::InvalidRequest);
        }
        if inventory.prepared.iter().any(|prepared| {
            prepared.local_pairing.parts().server == server
                && !inventory.is_suppressed(&prepared.local_pairing)
        }) || inventory
            .enrollment_acknowledgements
            .iter()
            .any(|acknowledgement| acknowledgement.prepared.local_pairing.parts().server == server)
        {
            return Err(PairingFailure::RecoveryRequired);
        }
        let (_, device) = self
            .claim_credential(&namespace, CredentialPurpose::DeviceIdentity)
            .await?;
        let (generation, authentication) = self
            .claim_credential(
                &namespace,
                CredentialPurpose::PairingAuthentication { server },
            )
            .await?;
        let slot = self
            .journal
            .snapshot()
            .map_err(classify)?
            .pairing()
            .slot(authentication.scope.reference())
            .cloned()
            .ok_or(PairingFailure::CredentialIntegrity)?;
        let reference = match slot.route_reference {
            Some(reference) if slot.route.as_ref() == Some(&route) => reference,
            Some(_) => return Err(PairingFailure::RouteChanged),
            None => {
                let mut bytes = [0; 16];
                SystemRandom::new()
                    .fill(&mut bytes)
                    .map_err(|_| PairingFailure::CredentialUnavailable)?;
                let reference = RevocationRouteReference::new(bytes);
                self.journal
                    .transact(vec![ClientJournalChange::BindCredentialRoute {
                        reference: authentication.scope.reference(),
                        route_reference: reference,
                        route,
                    }])
                    .await
                    .map_err(classify)?;
                reference
            }
        };
        let verifier = DirectEnrollmentAttempt::new(pin, reference, generation.get())?;
        let verified = verifier.confirm(pin, reference, generation.get(), compared_fingerprint)?;
        let hash = ring::digest::digest(&ring::digest::SHA256, &device.public_key);
        let mut device_id = [0; 16];
        device_id.copy_from_slice(&hash.as_ref()[..16]);
        let mut nonce = [0; 32];
        SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| PairingFailure::CredentialUnavailable)?;
        let proposal = EnrollmentProposal::new(EnrollmentProposalParts {
            server,
            device: DeviceId::new(device_id),
            keys: PairingKeys::new(PairingKeysParts {
                device_identity_public_key: device.public_key,
                pairing_authentication_public_key: authentication.public_key,
            })
            .map_err(|_| PairingFailure::InvalidRequest)?,
            claim_nonce: nonce,
        })
        .map_err(|_| PairingFailure::InvalidRequest)?;
        let (device_proof, authentication_proof) = self
            .claim_proofs(&verified, &proposal, &device, &authentication)
            .await?;
        Ok(PublicDeviceClaim {
            schema: 1,
            server_identity: pin.identity,
            server_public_key: pin.public_key,
            proposal,
            device_proof: device_proof.to_vec(),
            authentication_proof: authentication_proof.to_vec(),
        })
    }

    async fn claim_credential(
        &self,
        namespace: &CredentialNamespace,
        purpose: CredentialPurpose,
    ) -> Result<(LocalPairingGeneration, PreparedCredential), PairingFailure> {
        let inventory = self.inventory().await?;
        let mut candidates = inventory
            .credential_preparations
            .iter()
            .filter(|preparation| {
                &preparation.scope.parts().namespace == namespace
                    && preparation.scope.parts().purpose == purpose
                    && (purpose == CredentialPurpose::DeviceIdentity
                        || !inventory
                            .saved
                            .iter()
                            .any(|saved| saved.credential().scope == preparation.scope))
            });
        let candidate = candidates.next();
        if candidates.next().is_some() {
            return Err(PairingFailure::RecoveryRequired);
        }
        if let Some(candidate) = candidate {
            return Ok((
                candidate.generation,
                self.recover_credential(candidate.generation)
                    .await?
                    .ok_or(PairingFailure::CredentialUnavailable)?,
            ));
        }
        let scope = DeviceCredentials::new_scope(namespace.clone(), purpose)?;
        self.prepare_credential(scope).await
    }

    async fn claim_proofs(
        &self,
        verified: &VerifiedServer,
        proposal: &EnrollmentProposal,
        device: &PreparedCredential,
        authentication: &PreparedCredential,
    ) -> Result<([u8; 64], [u8; 64]), PairingFailure> {
        let device_use = verified.credential_use(device)?;
        let authentication_use = verified.credential_use(authentication)?;
        let device_key = self.credentials.signing_key(device, &device_use).await?;
        let authentication_key = self
            .credentials
            .signing_key(authentication, &authentication_use)
            .await?;
        VerifiedDeviceClaim::proofs(proposal, device_key.as_ref(), authentication_key.as_ref())
            .map_err(|_| PairingFailure::CredentialIntegrity)
    }
}
