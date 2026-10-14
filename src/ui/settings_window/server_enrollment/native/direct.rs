//! Transient invitation import. The bearer stays in zeroizing buffers and every
//! key must already belong to the native credential owner of this journal.

use super::*;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use spaceterm_client::{api::RouteParts, pairing::credentials::CredentialPurpose};
use spaceterm_core::{
    identity::ServerId,
    operations::{OperationBinding, OperationId, OperationSequence},
    pairing::{LocalPairingKey, LocalPairingKeyParts, RevocationRouteReference},
};
use spaceterm_protocol::authentication::ServerPin;

pub(crate) struct NativeDirectInvitation;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Invitation<'a> {
    schema: u16,
    server_identity: [u8; 16],
    server_public_key: [u8; 32],
    #[serde(borrow)]
    authorization_delivery: &'a str,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ServerDescriptor {
    schema: u16,
    server_identity: [u8; 16],
    server_public_key: [u8; 32],
}

impl DirectInvitationPreparation for NativeDirectInvitation {
    fn prepare_claim<'a>(
        &'a self,
        address: String,
        descriptor: Zeroizing<String>,
        compared_fingerprint: String,
        attempt: EnrollmentAttempt,
        pairing: &'a NativePairingRuntime,
    ) -> PairingFuture<'a, Result<String, EnrollmentFailure>> {
        Box::pin(async move {
            attempt.validate()?;
            if address.is_empty()
                || address.len() > 2048
                || address.chars().any(char::is_control)
                || descriptor.is_empty()
                || descriptor.len() > 4096
                || compared_fingerprint.len() > 96
            {
                return Err(PairingFailure::InvalidRequest.into());
            }
            let descriptor: ServerDescriptor =
                serde_json::from_str(&descriptor).map_err(|_| PairingFailure::InvalidRequest)?;
            if descriptor.schema != 1 {
                return Err(PairingFailure::InvalidRequest.into());
            }
            let pin = ServerPin {
                identity: descriptor.server_identity,
                public_key: descriptor.server_public_key,
            };
            if spaceterm_client::pairing::enrollment::fingerprint(pin.public_key)
                != compared_fingerprint
            {
                return Err(PairingFailure::FingerprintMismatch.into());
            }
            let route = route(&address).await?;
            attempt.validate()?;
            let namespace = spaceterm_client::pairing::credentials::CredentialNamespace::new(
                if cfg!(spaceterm_packaged) && cfg!(spaceterm_release) {
                    "app.spaceterm.desktop"
                } else if cfg!(spaceterm_packaged) {
                    "app.spaceterm.preflight.desktop"
                } else {
                    "dev.spaceterm.desktop"
                }
                .into(),
            )?;
            let claim = pairing
                .prepare_public_claim(namespace, pin, route, &compared_fingerprint)
                .await?;
            attempt.validate()?;
            serde_json::to_string(&claim).map_err(|_| PairingFailure::InvalidDurableState.into())
        })
    }
    fn prepare<'a>(
        &'a self,
        address: String,
        invitation: Zeroizing<String>,
        attempt: EnrollmentAttempt,
        pairing: &'a NativePairingRuntime,
    ) -> PairingFuture<'a, Result<DirectPreparation, EnrollmentFailure>> {
        Box::pin(async move {
            attempt.validate()?;
            if address.is_empty()
                || address.len() > 2048
                || address.chars().any(char::is_control)
                || invitation.is_empty()
                || invitation.len() > 65_536
            {
                return Err(PairingFailure::InvalidRequest.into());
            }
            let imported: Invitation<'_> =
                serde_json::from_str(&invitation).map_err(|_| PairingFailure::InvalidRequest)?;
            if imported.schema != 1
                || imported.server_identity == [0; 16]
                || imported.server_public_key == [0; 32]
            {
                return Err(PairingFailure::InvalidRequest.into());
            }
            let delivery = Zeroizing::new(
                STANDARD
                    .decode(imported.authorization_delivery)
                    .map_err(|_| PairingFailure::InvalidRequest)?,
            );
            let (reservation, authorization) = EnrollmentAuthorization::from_delivery(delivery)
                .map_err(|_| PairingFailure::InvalidRequest)?;
            let pin = ServerPin {
                identity: imported.server_identity,
                public_key: imported.server_public_key,
            };
            let server = ServerId::new(pin.identity);
            if reservation.parts().target.parts().server != server {
                return Err(PairingFailure::IdentityMismatch.into());
            }
            let route = route(&address).await?;
            attempt.validate()?;
            let inventory = pairing.inventory().await?;
            let device = recover_key(
                pairing,
                &inventory,
                CredentialPurpose::DeviceIdentity,
                reservation.parts().keys.parts().device_identity_public_key,
            )
            .await?;
            attempt.validate()?;
            let authentication = inventory
                .credential_preparations
                .iter()
                .find(|preparation| {
                    preparation.scope.parts().purpose
                        == CredentialPurpose::PairingAuthentication { server }
                        && preparation.credential.as_ref().is_some_and(|credential| {
                            credential.public_key
                                == reservation
                                    .parts()
                                    .keys
                                    .parts()
                                    .pairing_authentication_public_key
                        })
                })
                .ok_or(PairingFailure::CredentialUnavailable)?;
            let credential = pairing
                .recover_credential(authentication.generation)
                .await?
                .ok_or(PairingFailure::CredentialUnavailable)?;
            attempt.validate()?;
            if credential.scope.parts().namespace != device.scope.parts().namespace {
                return Err(PairingFailure::CredentialIntegrity.into());
            }
            let state = pairing.journal().snapshot().map_err(classify)?;
            let slot = state
                .pairing()
                .slot(credential.scope.reference())
                .ok_or(PairingFailure::CredentialIntegrity)?;
            let reference = match slot.route_reference {
                Some(reference) if slot.route.as_ref() == Some(&route) => reference,
                Some(_) => return Err(PairingFailure::RouteChanged.into()),
                None => {
                    let mut reference = [0; 16];
                    getrandom::fill(&mut reference).map_err(|_| PairingFailure::Offline)?;
                    RevocationRouteReference::new(reference)
                }
            };
            let parts = reservation.parts();
            let prepared = PreparedEnrollment {
                local_pairing: LocalPairingKey::new(LocalPairingKeyParts {
                    server,
                    pairing: parts.target.parts().pairing,
                    generation: authentication.generation,
                })
                .map_err(|_| PairingFailure::InvalidRequest)?,
                device: parts.target.parts().device,
                pin,
                keys: parts.keys.clone(),
                client: parts.initial_client,
                credential,
                route: reference,
                enrollment: OperationBinding {
                    id: OperationId::new(
                        parts.enrollment_stream,
                        OperationSequence::new(1).map_err(|_| PairingFailure::InvalidRequest)?,
                    ),
                    digest: parts.enrollment_request_digest,
                },
                reservation: Some(reservation),
            };
            prepared.validate()?;
            Ok(DirectPreparation {
                prepared,
                device,
                authorization: Some(authorization),
                route,
                majors: spaceterm_protocol::negotiation::SUPPORTED_MAJORS.to_vec(),
            })
        })
    }
}

async fn recover_key(
    pairing: &NativePairingRuntime,
    inventory: &spaceterm_client::pairing::journal::PairingJournalSnapshot,
    purpose: CredentialPurpose,
    public: [u8; 32],
) -> Result<PreparedCredential, PairingFailure> {
    let preparation = inventory
        .credential_preparations
        .iter()
        .find(|preparation| {
            preparation.scope.parts().purpose == purpose
                && preparation
                    .credential
                    .as_ref()
                    .is_some_and(|credential| credential.public_key == public)
        })
        .ok_or(PairingFailure::CredentialUnavailable)?;
    let credential = pairing
        .recover_credential(preparation.generation)
        .await?
        .ok_or(PairingFailure::CredentialUnavailable)?;
    if credential.public_key != public {
        return Err(PairingFailure::CredentialIntegrity);
    }
    Ok(credential)
}

async fn route(address: &str) -> Result<RouteDescription, EnrollmentFailure> {
    let authority = address
        .strip_prefix("wss://")
        .ok_or(PairingFailure::InvalidRequest)?;
    let authority = authority.strip_suffix('/').unwrap_or(authority);
    if authority.contains(['/', '?', '#', '@']) {
        return Err(PairingFailure::InvalidRequest.into());
    }
    let (host, port) = authority
        .rsplit_once(':')
        .ok_or(PairingFailure::InvalidRequest)?;
    let port: u16 = port.parse().map_err(|_| PairingFailure::InvalidRequest)?;
    let host = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    if host.is_empty() || port == 0 {
        return Err(PairingFailure::InvalidRequest.into());
    }
    let address = tokio::time::timeout(
        spaceterm_client::limits::DIAL_DEADLINE,
        tokio::net::lookup_host((host, port)),
    )
    .await
    .map_err(|_| PairingFailure::TimedOut)?
    .map_err(|_| PairingFailure::Offline)?
    .next()
    .ok_or(PairingFailure::Offline)?;
    RouteDescription::new(RouteParts::Wss {
        address,
        server_name: host.to_owned(),
    })
    .map_err(|_| PairingFailure::InvalidRequest.into())
}
