//! Local account-owner exchange over a separately verified private endpoint.
//! The ordinary desktop binding is published only by the portable enrollment driver.

mod channel;
#[cfg(test)]
mod recovery_tests;

use super::classify;
use channel::{NativeOwnerChannel, OwnerChannelProvider};
use spaceterm_client::{
    failure::{ClientFailure, ClientFailureClass},
    journal::{ClientJournalChange, JournalController},
    pairing::{
        credentials::{
            CredentialNamespace, CredentialPurpose, NativeDeviceCredentials, PairingFuture,
        },
        enrollment::{
            DirectEnrollmentAttempt, EnrollmentAttemptRequest, EnrollmentConnector,
            EnrollmentDriver, PairingTransport,
        },
        journal::{
            JournalPairingStore, LocalPairingStore, PreparedEnrollment, prepare_native_credential,
            recover_native_credential,
        },
    },
};
use spaceterm_client_native::{NativeClientProvider, credentials::adapter::DeviceCredentials};
use spaceterm_core::{
    identity::{DeviceId, ServerId},
    operations::{OperationBinding, OperationId, OperationSequence},
    pairing::{
        EnrollmentProposal, EnrollmentProposalParts, LocalPairingKey, LocalPairingKeyParts,
        PairingFailure, PairingKeys, PairingKeysParts, PairingOutcome, RevocationRouteReference,
    },
};
use spaceterm_host::{
    ipc::{IpcTransport, PrivateIpcDirectory},
    startup::{BuildIdentity, DesiredServerState},
    transport::{MessageLimit, MessageTransport},
};
use spaceterm_protocol::{
    authentication::ServerPin,
    pairing_adapter::{
        EnrollmentAuthorization, PublicEnrollmentReservation, VerifiedDeviceClaim,
        enrollment_proposal_digest,
    },
};
use std::{sync::Arc, time::Duration};
use zeroize::Zeroizing;

const FRAME_LIMIT: usize = 64 * 1024;
const DEADLINE: Duration = Duration::from_secs(15);

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnerReply {
    schema: u16,
    disposition: Disposition,
    outcome: PairingOutcome,
}
#[derive(serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum Disposition {
    AuthorizationDelivered,
    AuthorizationReplyLost,
    AuthorizationInvalidated,
    ReceiptAvailable,
}

pub(super) async fn enroll(
    native: &NativeClientProvider,
    journal: Arc<JournalController>,
) -> Result<(), ClientFailure> {
    enroll_with_credentials(native, journal, DeviceCredentials::native()).await
}

pub(crate) async fn enroll_with_credentials<N: NativeDeviceCredentials + Clone + 'static>(
    native: &NativeClientProvider,
    journal: Arc<JournalController>,
    credentials: N,
) -> Result<(), ClientFailure> {
    enroll_using(
        native,
        journal,
        credentials,
        &NativeOwnerChannel(native.clone()),
    )
    .await
}

async fn enroll_using<N: NativeDeviceCredentials + Clone + 'static>(
    native: &NativeClientProvider,
    journal: Arc<JournalController>,
    credentials: N,
    channels: &dyn OwnerChannelProvider,
) -> Result<(), ClientFailure> {
    let provisioning_credentials = Arc::new(credentials.clone());
    let management = native.management();
    let supervisor = management.supervisor();
    let registration = supervisor
        .discover()
        .map_err(classify)?
        .ok_or_else(|| failure(ClientFailureClass::StaleAuthority))?;
    // Running uses the existing actor's private owner endpoint. Stopped uses
    // the deliberate listener-free owner command, never service-manager Start.
    let pin = registration.server_pin();
    let server = ServerId::new(pin.identity);
    let ordinary = spaceterm_client_native::route::LocalRouteAuthority::new(
        registration.endpoint().map_err(classify)?,
    )?;
    let route = ordinary.description().clone();
    let transport = channels.open(&registration).await?;
    let result = async {
        let namespace = CredentialNamespace::new(
            match native.key().build_identity {
                BuildIdentity::Release => "app.spaceterm.desktop",
                BuildIdentity::Preflight => "app.spaceterm.preflight.desktop",
                BuildIdentity::Development => "dev.spaceterm.desktop",
            }
            .to_owned(),
        )
        .map_err(pairing_failure)?;
        let store = JournalPairingStore::new(journal.clone());
        let inventory = store.recover().await.map_err(pairing_failure)?;
        let device = if let Some(preparation) =
            inventory
                .credential_preparations
                .iter()
                .find(|preparation| {
                    preparation.scope.parts().namespace == namespace
                        && preparation.scope.parts().purpose == CredentialPurpose::DeviceIdentity
                }) {
            recover_native_credential(&store, &credentials, preparation.generation)
                .await
                .map_err(pairing_failure)?
                .ok_or_else(|| failure(ClientFailureClass::StorageUnavailable))?
        } else {
            let scope =
                DeviceCredentials::new_scope(namespace.clone(), CredentialPurpose::DeviceIdentity)
                    .map_err(pairing_failure)?;
            prepare_native_credential(&store, &credentials, scope)
                .await
                .map_err(pairing_failure)?
                .1
        };
        let pending = inventory
            .credential_preparations
            .iter()
            .find(|preparation| {
                preparation.scope.parts().namespace == namespace
                    && preparation.scope.parts().purpose
                        == CredentialPurpose::PairingAuthentication { server }
            });
        let retained_preparation = inventory.prepared.iter().find(|prepared| {
            prepared.pin == pin
                && prepared.credential.scope.parts().namespace == namespace
                && !inventory.is_suppressed(&prepared.local_pairing)
        });
        let (generation, authentication) = if let Some(prepared) = retained_preparation {
            let credential = credentials
                .recover_preparation(&prepared.credential.scope)
                .await
                .map_err(pairing_failure)?
                .ok_or_else(|| failure(ClientFailureClass::StorageUnavailable))?;
            if credential != prepared.credential {
                return Err(failure(ClientFailureClass::InvalidStorage));
            }
            (prepared.local_pairing.parts().generation, credential)
        } else if let Some(pending) = pending {
            let credential = recover_native_credential(&store, &credentials, pending.generation)
                .await
                .map_err(pairing_failure)?
                .ok_or_else(|| failure(ClientFailureClass::StorageUnavailable))?;
            (pending.generation, credential)
        } else {
            let scope = DeviceCredentials::new_scope(
                namespace,
                CredentialPurpose::PairingAuthentication { server },
            )
            .map_err(pairing_failure)?;
            prepare_native_credential(&store, &credentials, scope)
                .await
                .map_err(pairing_failure)?
        };
        let keys = PairingKeys::new(PairingKeysParts {
            device_identity_public_key: device.public_key,
            pairing_authentication_public_key: authentication.public_key,
        })
        .map_err(|_| failure(ClientFailureClass::InvalidStorage))?;
        let original = supervisor
            .retained_owner_proposal(server, &keys)
            .map_err(classify)?;
        let (request, proposal) = if let Some(original) = original {
            original
        } else {
            // Legacy incomplete work has no retained claim. It cannot authorize a
            // replacement ticket merely because the original observer disappeared.
            if pending.is_some() || retained_preparation.is_some() {
                return Err(failure(ClientFailureClass::RecoveryRequired));
            }
            let mut nonce = [0; 32];
            getrandom::fill(&mut nonce).map_err(|_| failure(ClientFailureClass::Unavailable))?;
            let mut device_id = [0; 16];
            let hash = ring::digest::digest(&ring::digest::SHA256, &device.public_key);
            device_id.copy_from_slice(&hash.as_ref()[..16]);
            let proposal = EnrollmentProposal::new(EnrollmentProposalParts {
                server,
                device: DeviceId::new(device_id),
                keys,
                claim_nonce: nonce,
            })
            .map_err(|_| failure(ClientFailureClass::InvalidStorage))?;
            let request = supervisor.allocate_request().map_err(classify)?;
            supervisor
                .record_owner_proposal(request, proposal.clone())
                .map_err(classify)?;
            (request, proposal)
        };
        let captured = journal.snapshot()?;
        let slot = captured
            .pairing()
            .slot(authentication.scope.reference())
            .cloned()
            .ok_or_else(|| failure(ClientFailureClass::InvalidStorage))?;
        let reference = match slot.route_reference {
            Some(reference)
                if slot.route.as_ref() == Some(&route)
                    || captured.pairing().prepared().iter().any(|prepared| {
                        prepared.route_reference == reference
                            && prepared.target.target().is_ok_and(|target| {
                                target.credential() == authentication.scope.reference()
                                    && target.route() == &route
                            })
                    }) =>
            {
                reference
            }
            Some(_) => return Err(failure(ClientFailureClass::StaleAuthority)),
            None => {
                let mut reference = [0; 16];
                getrandom::fill(&mut reference)
                    .map_err(|_| failure(ClientFailureClass::Unavailable))?;
                let reference = RevocationRouteReference::new(reference);
                journal
                    .transact(vec![ClientJournalChange::BindCredentialRoute {
                        reference: authentication.scope.reference(),
                        route_reference: reference,
                        route: route.clone(),
                    }])
                    .await?;
                reference
            }
        };
        let attempt = DirectEnrollmentAttempt::new(pin, reference, generation.get())
            .map_err(pairing_failure)?;
        // Local native registration is the out-of-band authority. No remote
        // fingerprint or saved route string grants this confirmation.
        let verified = attempt
            .confirm(pin, reference, generation.get(), &attempt.fingerprint())
            .map_err(pairing_failure)?;
        let device_signer = credentials
            .signing_key(
                &device,
                &verified.credential_use(&device).map_err(pairing_failure)?,
            )
            .await
            .map_err(pairing_failure)?;
        let auth_signer = credentials
            .signing_key(
                &authentication,
                &verified
                    .credential_use(&authentication)
                    .map_err(pairing_failure)?,
            )
            .await
            .map_err(pairing_failure)?;
        let (device_proof, authentication_proof) =
            VerifiedDeviceClaim::proofs(&proposal, device_signer.as_ref(), auth_signer.as_ref())
                .map_err(|_| failure(ClientFailureClass::AuthorizationDenied))?;
        let claim = enrollment_proposal_digest(&proposal)
            .map_err(|_| failure(ClientFailureClass::InvalidStorage))?;
        let digest = supervisor
            .owner_enrollment_digest(request, server, claim)
            .map_err(classify)?;
        supervisor
            .validate_owner_enrollment(&request, digest, server, claim)
            .map_err(classify)?;
        let initial = serde_json::to_vec(&serde_json::json!({
            "request": request,
            "proposal": proposal,
            "device_proof": device_proof.to_vec(),
            "authentication_proof": authentication_proof.to_vec(),
        }))
        .map_err(|_| failure(ClientFailureClass::InvalidStorage))?;
        send(&transport, &initial).await.map_err(pairing_failure)?;
        let reply: OwnerReply =
            serde_json::from_slice(&receive(&transport).await.map_err(pairing_failure)?)
                .map_err(|_| failure(ClientFailureClass::InvalidStorage))?;
        if reply.schema != 1
            || !matches!(reply.outcome, PairingOutcome::AuthorizationReserved { .. })
        {
            return Err(failure(ClientFailureClass::AuthorizationDenied));
        }
        let delivery = receive(&transport).await.map_err(pairing_failure)?;
        let (reservation, authorization) = match reply.disposition {
            Disposition::AuthorizationDelivered => {
                let (reservation, authorization) = EnrollmentAuthorization::from_delivery(delivery)
                    .map_err(|_| failure(ClientFailureClass::AuthorizationDenied))?;
                (reservation, Some(authorization))
            }
            Disposition::ReceiptAvailable => (
                PublicEnrollmentReservation::decode(&delivery)
                    .map_err(|_| failure(ClientFailureClass::InvalidStorage))?,
                None,
            ),
            Disposition::AuthorizationReplyLost | Disposition::AuthorizationInvalidated => {
                return Err(failure(ClientFailureClass::RecoveryRequired));
            }
        };
        let parts = reservation.parts();
        if parts.claim_digest != claim
            || parts.target.parts().server != server
            || parts.target.parts().device != proposal.parts().device
            || parts.keys != proposal.parts().keys
        {
            return Err(failure(ClientFailureClass::PinMismatch));
        }
        supervisor
            .validate_owner_enrollment(&request, digest, server, claim)
            .map_err(classify)?;
        let prepared = PreparedEnrollment {
            local_pairing: LocalPairingKey::new(LocalPairingKeyParts {
                server,
                pairing: parts.target.parts().pairing,
                generation,
            })
            .map_err(|_| failure(ClientFailureClass::InvalidStorage))?,
            device: proposal.parts().device,
            pin,
            keys: proposal.parts().keys.clone(),
            client: parts.initial_client,
            credential: authentication,
            route: reference,
            enrollment: OperationBinding {
                id: OperationId::new(
                    parts.enrollment_stream,
                    OperationSequence::new(1)
                        .map_err(|_| failure(ClientFailureClass::InvalidStorage))?,
                ),
                digest: parts.enrollment_request_digest,
            },
            reservation: Some(reservation.clone()),
        };
        prepared.validate().map_err(pairing_failure)?;
        if retained_preparation.is_none() {
            journal
                .transact(vec![ClientJournalChange::BindCredentialRoute {
                    reference: prepared.credential.scope.reference(),
                    route_reference: reference,
                    route,
                }])
                .await?;
        }
        let connector = OwnerConnector {
            pin,
            reference,
            transport: std::sync::Mutex::new(Some(transport.clone())),
        };
        let driver = EnrollmentDriver::new(store, credentials, connector);
        driver
            .enroll(EnrollmentAttemptRequest {
                verified: &verified,
                prepared: &prepared,
                reservation,
                device: &device,
                authorization: authorization.as_ref(),
                majors: spaceterm_protocol::negotiation::SUPPORTED_MAJORS.to_vec(),
            })
            .await
            .map_err(pairing_failure)?;
        let inventory = journal.snapshot()?;
        if inventory
            .pairing()
            .acknowledgements()
            .iter()
            .any(|ack| ack.prepared.enrollment.id == prepared.enrollment.id)
        {
            return Err(failure(ClientFailureClass::RecoveryRequired));
        }
        Ok(())
    }
    .await;
    let closed = transport
        .close()
        .await
        .map_err(spaceterm_client_native::route::classify);
    result.and(closed)?;
    // Dedicated Clients need the ordinary actor. Stopped enrollment must not
    // start it or invalidate the completed listener-free owner transaction.
    if registration.desired_state() == spaceterm_host::startup::DesiredServerState::Stopped {
        return Ok(());
    }
    spaceterm_client_native::native::consumer::NativeConsumer::new(
        native.management(),
        spaceterm_client_native::native::consumer::NativeConsumerKind::Cli,
    )
    .with_credentials(provisioning_credentials)
    .provision_journal(journal)
    .await
    .map(|_| ())
    .map_err(|error| {
        use spaceterm_client::management_cli::ManagementFailure;
        failure(match error {
            ManagementFailure::CredentialUnavailable | ManagementFailure::StorageUnavailable => {
                ClientFailureClass::StorageUnavailable
            }
            ManagementFailure::AuthorizationDenied | ManagementFailure::Revoked => {
                ClientFailureClass::AuthorizationDenied
            }
            ManagementFailure::OutcomePending => ClientFailureClass::RecoveryRequired,
            _ => ClientFailureClass::Unavailable,
        })
    })
}

struct OwnerConnector {
    pin: ServerPin,
    reference: RevocationRouteReference,
    transport: std::sync::Mutex<Option<Arc<dyn MessageTransport>>>,
}
impl EnrollmentConnector for OwnerConnector {
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
            Ok(Box::new(OwnerTransport(transport)) as Box<dyn PairingTransport>)
        })
    }
}
struct OwnerTransport(Arc<dyn MessageTransport>);
impl PairingTransport for OwnerTransport {
    fn send<'a>(&'a mut self, bytes: &'a [u8]) -> PairingFuture<'a, Result<(), PairingFailure>> {
        Box::pin(send(&self.0, bytes))
    }
    fn receive(&mut self) -> PairingFuture<'_, Result<Zeroizing<Vec<u8>>, PairingFailure>> {
        Box::pin(receive(&self.0))
    }
}
async fn send(transport: &Arc<dyn MessageTransport>, bytes: &[u8]) -> Result<(), PairingFailure> {
    tokio::time::timeout(DEADLINE, transport.send(bytes))
        .await
        .map_err(|_| PairingFailure::TimedOut)?
        .map_err(|_| PairingFailure::Offline)
}
async fn receive(
    transport: &Arc<dyn MessageTransport>,
) -> Result<Zeroizing<Vec<u8>>, PairingFailure> {
    tokio::time::timeout(DEADLINE, transport.recv())
        .await
        .map_err(|_| PairingFailure::TimedOut)?
        .map_err(|_| PairingFailure::Offline)?
        .filter(|bytes| !bytes.is_empty() && bytes.len() <= FRAME_LIMIT)
        .map(Zeroizing::new)
        .ok_or(PairingFailure::Offline)
}
fn failure(class: ClientFailureClass) -> ClientFailure {
    ClientFailure::new(class)
}
fn pairing_failure(error: PairingFailure) -> ClientFailure {
    failure(match error {
        PairingFailure::CredentialUnavailable => ClientFailureClass::StorageUnavailable,
        PairingFailure::IdentityMismatch | PairingFailure::FingerprintMismatch => {
            ClientFailureClass::PinMismatch
        }
        PairingFailure::AuthorizationDenied
        | PairingFailure::AuthorizationRequired
        | PairingFailure::PairingRevoked => ClientFailureClass::AuthorizationDenied,
        PairingFailure::RecoveryRequired => ClientFailureClass::RecoveryRequired,
        PairingFailure::LimitExceeded => ClientFailureClass::ResourceLimit,
        PairingFailure::CredentialIntegrity | PairingFailure::ReceiptInvalid => {
            ClientFailureClass::InvalidStorage
        }
        _ => ClientFailureClass::Unavailable,
    })
}

#[cfg(test)]
mod tests;
