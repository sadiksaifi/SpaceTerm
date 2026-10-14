//! Public preparation intent is retained before native credential insertion.

use super::*;
use spaceterm_client::pairing::credentials::CredentialScope;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OwnerPreparation {
    pub server: ServerId,
    pub server_public_key: [u8; 32],
    pub device_scope: CredentialScope,
    pub authentication_scope: CredentialScope,
    pub device: DeviceId,
    pub claim_nonce: [u8; 32],
    pub request: NativeRequestId,
    pub reference: RevocationRouteReference,
}

impl OwnerPreparation {
    pub(super) fn new(
        registration: &RegisteredServer,
        native: &NativeServiceManagement,
    ) -> Result<Self, ManagementFailure> {
        let pin = registration.server_pin();
        let server = ServerId::new(pin.identity);
        let namespace = CredentialNamespace::new(format!(
            "spaceterm-native-{}",
            match registration.key().build_identity {
                BuildIdentity::Release => "release",
                BuildIdentity::Preflight => "preflight",
                BuildIdentity::Development => "development",
            }
        ))
        .map_err(classify_pairing)?;
        Ok(Self {
            server,
            server_public_key: pin.public_key,
            device_scope: DeviceCredentials::new_scope(
                namespace.clone(),
                CredentialPurpose::DeviceIdentity,
            )
            .map_err(classify_pairing)?,
            authentication_scope: DeviceCredentials::new_scope(
                namespace,
                CredentialPurpose::PairingAuthentication { server },
            )
            .map_err(classify_pairing)?,
            device: DeviceId::new(random()?),
            claim_nonce: random()?,
            request: native
                .supervisor
                .allocate_request()
                .map_err(classify_service)?,
            reference: RevocationRouteReference::new(random()?),
        })
    }

    pub(super) async fn prepare(
        &self,
        registration: &RegisteredServer,
        pairing: &NativePairingRuntime,
    ) -> Result<OwnerAttempt, ManagementFailure> {
        if self.server.bytes() != registration.server_pin().identity
            || self.server_public_key != registration.server_pin().public_key
            || self.device_scope.parts().purpose != CredentialPurpose::DeviceIdentity
            || self.authentication_scope.parts().purpose
                != (CredentialPurpose::PairingAuthentication {
                    server: self.server,
                })
            || self.device_scope.parts().namespace != self.authentication_scope.parts().namespace
            || self.device_scope.reference() == self.authentication_scope.reference()
        {
            return Err(ManagementFailure::AuthorizationDenied);
        }
        let (_, device) = pairing
            .resume_credential(self.device_scope.clone())
            .await
            .map_err(classify_pairing)?;
        let (generation, authentication) = pairing
            .resume_credential(self.authentication_scope.clone())
            .await
            .map_err(classify_pairing)?;
        Ok(OwnerAttempt {
            server_public_key: self.server_public_key,
            proposal: EnrollmentProposal::new(EnrollmentProposalParts {
                server: self.server,
                device: self.device,
                keys: PairingKeys::new(PairingKeysParts {
                    device_identity_public_key: device.public_key,
                    pairing_authentication_public_key: authentication.public_key,
                })
                .map_err(|_| ManagementFailure::UnsafeNativeState)?,
                claim_nonce: self.claim_nonce,
            })
            .map_err(|_| ManagementFailure::UnsafeNativeState)?,
            request: self.request,
            generation,
            device,
            authentication,
            reference: self.reference,
        })
    }
}

pub(super) async fn retained_preparation(
    native: &NativeServiceManagement,
    registration: &RegisteredServer,
    pairing: &NativePairingRuntime,
    admission: &ClientProvisioning,
    consumer: &str,
) -> Result<OwnerAttempt, ManagementFailure> {
    let preparation_name = match consumer {
        "enrollment" => "enrollment-preparation",
        "desktop" => "desktop-preparation",
        _ => return Err(ManagementFailure::UnsafeNativeState),
    };
    let retained = admission
        .owner_attempt(preparation_name)
        .map_err(classify_service)?;
    let preparation: OwnerPreparation = if let Some(bytes) = retained {
        serde_json::from_slice(&bytes).map_err(|_| ManagementFailure::UnsafeNativeState)?
    } else {
        let inventory = pairing.inventory().await.map_err(classify_pairing)?;
        // An older partial attempt without exact retained scopes cannot safely
        // allocate a replacement. Its original native insertion may still finish.
        if !inventory.prepared.is_empty() || !inventory.credential_preparations.is_empty() {
            return Err(ManagementFailure::OutcomePending);
        }
        let preparation = OwnerPreparation::new(registration, native)?;
        let bytes =
            serde_json::to_vec(&preparation).map_err(|_| ManagementFailure::UnsafeNativeState)?;
        admission
            .retain_owner_attempt(preparation_name, None, &bytes)
            .map_err(classify_service)?;
        preparation
    };
    preparation.prepare(registration, pairing).await
}
