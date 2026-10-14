//! Deliberate local owner enrollment and dedicated native Client provisioning.
//! Account/build authority chooses every directory and IPC endpoint.

use super::*;
use crate::{
    cli::{arguments::PublicFingerprint, output::CliOutput, runner::CliIo},
    pairing_runtime::OwnerEnrollmentRequest,
};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use spaceterm_client::pairing::{
    credentials::{CredentialNamespace, CredentialPurpose, PairingFuture, PreparedCredential},
    enrollment::{DirectEnrollmentAttempt, EnrollmentConnector, PairingTransport},
    journal::PreparedEnrollment,
};
use spaceterm_core::{
    identity::{DeviceId, ServerId},
    operations::{Command, OperationBinding, OperationSequence, Outcome},
    pairing::{
        EnrollmentProposal, EnrollmentProposalParts, LocalPairingKeyParts, PairingCommand,
        PairingKeys, PairingKeysParts, PairingOutcome, RevocationRouteReference,
    },
};
use spaceterm_host::{
    account_service::ClientProvisioning,
    ipc::{IpcTransport, PrivateIpcDirectory},
    native_deployment::contracts::NativeRequestId,
    startup::{BuildIdentity, DesiredServerState},
    transport::{MessageLimit, MessageTransport},
};
use spaceterm_protocol::pairing_adapter::{
    EnrollmentAuthorization, VerifiedDeviceClaim, enrollment_proposal_digest,
};
use std::time::Duration;
use zeroize::Zeroizing;

const OWNER_LIMIT: usize = 64 * 1024;
#[derive(Serialize)]
struct OwnerRequest {
    request: NativeRequestId,
    proposal: EnrollmentProposal,
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

impl NativeConsumer {
    pub(super) async fn enroll_registered(
        &self,
        expected: Option<PublicFingerprint>,
        recover_account: bool,
        io: &mut dyn CliIo,
    ) -> Result<CliOutput, ManagementFailure> {
        self.mutable()?;
        let _mutation = self.mutation.lock().await;
        let registration = self
            .native
            .supervisor
            .discover()
            .map_err(classify_service)?
            .ok_or(ManagementFailure::NativeUnavailable)?;
        let pin = registration.server_pin();
        match expected {
            Some(expected) if expected.bytes() != pin.public_key => {
                return Err(ManagementFailure::AuthorizationDenied);
            }
            Some(_) => {}
            None => {
                io.confirm_server_fingerprint(PublicFingerprint::from_public_key(pin.public_key))?
            }
        }
        if recover_account {
            // Repairing an existing account pairing is a separate owner action.
            return Err(ManagementFailure::AuthorizationUnavailable);
        }
        if registration.desired_state() != DesiredServerState::Running
            || !registration.is_installed()
        {
            return Err(ManagementFailure::NativeUnavailable);
        }
        // A desktop enrollment may already have provisioned this CLI. A
        // deliberate repeat must not create another Paired Device or replace it.
        match self.owner().await {
            Ok(owner) => {
                let inventory = owner.pairing.inventory().await.map_err(classify_pairing)?;
                if let [saved] = inventory.saved.as_slice() {
                    if saved.pin() != pin {
                        return Err(ManagementFailure::AuthorizationDenied);
                    }
                    owner
                        .pairing
                        .restore(saved.key(), saved.client().parts().client.id)
                        .await
                        .map_err(classify_pairing)?;
                    for name in ["companion", "management"] {
                        let codec = Arc::new(CanonicalCodec);
                        let verifier = Arc::new(PinnedReceipts);
                        let limits = JournalLimits::default();
                        let storage = NativeClientJournal::open_existing(
                            self.native
                                .supervisor
                                .client_connections_directory()
                                .join(name),
                            codec.clone(),
                            verifier.clone(),
                            limits,
                        )
                        .await
                        .map_err(|_| ManagementFailure::OutcomePending)?;
                        let journal =
                            JournalController::recover(Arc::new(storage), codec, verifier, limits)
                                .await
                                .map_err(classify_client)?;
                        let state = journal.snapshot().map_err(classify_client)?;
                        let [other] = state.bindings() else {
                            return Err(ManagementFailure::OutcomePending);
                        };
                        let binding = other.binding().map_err(classify_client)?;
                        if binding.target().pin() != pin
                            || binding.target().pairing() != saved.key().parts().pairing
                            || !journal
                                .ordinary_use_allowed(&binding)
                                .map_err(classify_client)?
                        {
                            return Err(ManagementFailure::OutcomePending);
                        }
                    }
                    return Ok(CliOutput::Enrolled {
                        server: ServerId::new(pin.identity),
                        client: saved.client().parts().client.id,
                    });
                }
                if !inventory.revocations.is_empty() {
                    return Err(ManagementFailure::Revoked);
                }
            }
            Err(ManagementFailure::PairingRequired) => {}
            Err(error) => return Err(error),
        }
        let admission = self
            .native
            .supervisor
            .admit_client_provisioning()
            .map_err(classify_service)?;
        let journal = open_journal(&admission, "enrollment").await?;
        let route = LocalRouteAuthority::new(registration.endpoint().map_err(classify_service)?)
            .map_err(classify_client)?;
        let description = route.description().clone();
        let owner = pairing_owner(
            journal,
            self.credentials.clone(),
            NativeRouteConnector::new(vec![route], None).map_err(classify_client)?,
        );
        let inventory = owner.pairing.inventory().await.map_err(classify_pairing)?;
        if inventory.saved.is_empty() {
            if !inventory.prepared.is_empty() || !inventory.credential_preparations.is_empty() {
                return Err(ManagementFailure::OutcomePending);
            }
            enroll_owner(&self.native, &registration, &owner.pairing, description).await?;
        } else if inventory.saved.len() != 1 || inventory.saved[0].pin() != pin {
            return Err(ManagementFailure::UnsafeNativeState);
        }
        for acknowledgement in owner
            .pairing
            .inventory()
            .await
            .map_err(classify_pairing)?
            .enrollment_acknowledgements
        {
            owner
                .pairing
                .acknowledge_enrollment_once(acknowledgement.prepared.enrollment.id)
                .await
                .map_err(classify_pairing)?;
        }
        let client = self
            .provision_from_owner(&registration, &owner, &admission)
            .await?;
        Ok(CliOutput::Enrolled {
            server: ServerId::new(pin.identity),
            client,
        })
    }

    /// Deliberate native desktop enrollment uses the acknowledged desktop journal
    /// only to issue new Clients. Their later connections use separate journals.
    pub async fn provision_journal(
        &self,
        journal: Arc<JournalController>,
    ) -> Result<spaceterm_core::identity::ClientId, ManagementFailure> {
        let registration = self
            .native
            .supervisor
            .discover()
            .map_err(classify_service)?
            .ok_or(ManagementFailure::NativeUnavailable)?;
        let route = LocalRouteAuthority::new(registration.endpoint().map_err(classify_service)?)
            .map_err(classify_client)?;
        let owner = pairing_owner(
            journal,
            self.credentials.clone(),
            NativeRouteConnector::new(vec![route], None).map_err(classify_client)?,
        );
        self.provision_consumers(owner.pairing).await
    }
    /// Explicit desktop or Bootstrap pairing may provision the three native
    /// consumers from its acknowledged primary journal. Ordinary reconnect never
    /// calls this method and never borrows that originating Client identity.
    pub async fn provision_consumers(
        &self,
        originating: Arc<NativePairingRuntime>,
    ) -> Result<spaceterm_core::identity::ClientId, ManagementFailure> {
        self.mutable()?;
        let _mutation = self.mutation.lock().await;
        let registration = self
            .native
            .supervisor
            .discover()
            .map_err(classify_service)?
            .ok_or(ManagementFailure::NativeUnavailable)?;
        let admission = self
            .native
            .supervisor
            .admit_client_provisioning()
            .map_err(classify_service)?;
        let route = LocalRouteAuthority::new(registration.endpoint().map_err(classify_service)?)
            .map_err(classify_client)?;
        let owner = PairingOwner {
            pairing: originating,
            routes: Arc::new(
                NativeRouteConnector::new(vec![route], None).map_err(classify_client)?,
            ),
            invalidator: Arc::new(ConsumerInvalidator::default()),
        };
        self.provision_from_owner(&registration, &owner, &admission)
            .await
    }

    async fn provision_from_owner(
        &self,
        registration: &RegisteredServer,
        owner: &PairingOwner,
        admission: &ClientProvisioning,
    ) -> Result<spaceterm_core::identity::ClientId, ManagementFailure> {
        let inventory = owner.pairing.inventory().await.map_err(classify_pairing)?;
        let selected = inventory
            .saved
            .iter()
            .filter(|saved| saved.pin() == registration.server_pin())
            .collect::<Vec<_>>();
        let [original] = selected.as_slice() else {
            return Err(ManagementFailure::AuthorizationDenied);
        };
        let opened = open_pairing(
            NativeConsumerKind::Cli,
            owner,
            false,
            Some(registration.server_pin()),
        )
        .await?;
        let result = provision(registration, owner, original, &opened, admission).await;
        let closed = opened._runtime.shutdown().await.map_err(classify_client);
        match result {
            Err(error) => Err(error),
            Ok(client) => {
                closed?;
                Ok(client)
            }
        }
    }
}

async fn provision(
    registration: &RegisteredServer,
    owner: &PairingOwner,
    original: &spaceterm_client::pairing::journal::SavedPairing,
    opened: &OpenedConsumer,
    admission: &ClientProvisioning,
) -> Result<spaceterm_core::identity::ClientId, ManagementFailure> {
    let (target, keys) = match &original.receipt().parts().claims {
        PairingReceiptBody::Enrollment { target, keys, .. }
        | PairingReceiptBody::ClientBinding { target, keys, .. } => (target, keys),
        _ => return Err(ManagementFailure::UnsafeNativeState),
    };
    let original_binding = owner
        .pairing
        .restore(original.key(), original.client().parts().client.id)
        .await
        .map_err(classify_pairing)?
        .binding()
        .binding()
        .map_err(classify_client)?;
    let operations = opened
        .operations
        .as_ref()
        .ok_or(ManagementFailure::AuthorizationDenied)?;
    let mut cli = None;
    for name in ["cli", "companion", "management"] {
        let journal = open_journal(admission, name).await?;
        let route = LocalRouteAuthority::new(registration.endpoint().map_err(classify_service)?)
            .map_err(classify_client)?;
        let receiving = pairing_owner(
            journal,
            owner.pairing.credentials(),
            NativeRouteConnector::new(vec![route], None).map_err(classify_client)?,
        );
        let inventory = receiving
            .pairing
            .inventory()
            .await
            .map_err(classify_pairing)?;
        let saved = if let [saved] = inventory.saved.as_slice() {
            if saved.pin() != original.pin()
                || saved.key().parts().pairing != original.key().parts().pairing
            {
                return Err(ManagementFailure::AuthorizationDenied);
            }
            saved.clone()
        } else if inventory.saved.is_empty() {
            if !inventory.revocations.is_empty() {
                return Err(ManagementFailure::Revoked);
            }
            let assessment_ticket = operations
                .submit_command(Command::Pairing(PairingCommand::AssessUnpair {
                    target: target.clone(),
                }))
                .await
                .map_err(classify_client)?;
            let version = match operations
                .wait(&assessment_ticket)
                .await
                .map_err(classify_client)?
            {
                Outcome::Pairing(value) => match *value {
                    PairingOutcome::UnpairAssessed(value) => value.parts().authorization_version,
                    _ => return Err(ManagementFailure::UnsafeNativeState),
                },
                _ => return Err(ManagementFailure::UnsafeNativeState),
            };

            let proof = owner
                .pairing
                .prepare_additional_client(
                    original.key(),
                    original.client().parts().client.id,
                    version,
                    operations.next_operation().map_err(classify_client)?,
                )
                .await
                .map_err(classify_pairing)?;

            let ticket = operations
                .submit_additional_client(proof)
                .await
                .map_err(classify_client)?;
            let receipt = match operations.wait(&ticket).await.map_err(classify_client)? {
                Outcome::Pairing(value) => match *value {
                    PairingOutcome::ClientBound(receipt) => receipt,
                    _ => return Err(ManagementFailure::UnsafeNativeState),
                },
                _ => return Err(ManagementFailure::UnsafeNativeState),
            };

            let PairingReceiptBody::ClientBinding {
                client, operation, ..
            } = &receipt.parts().claims
            else {
                return Err(ManagementFailure::UnsafeNativeState);
            };
            let (generation, credential) = receiving
                .pairing
                .retain_existing_credential(
                    original.credential().scope.clone(),
                    original.credential().public_key,
                )
                .await
                .map_err(classify_pairing)?;

            let prepared = PreparedEnrollment {
                local_pairing: LocalPairingKey::new(LocalPairingKeyParts {
                    server: target.parts().server,
                    pairing: target.parts().pairing,
                    generation,
                })
                .map_err(|_| ManagementFailure::UnsafeNativeState)?,
                device: target.parts().device,
                pin: original.pin(),
                keys: keys.clone(),
                client: client.parts().client.id,
                credential,
                route: route_reference(owner.pairing.journal(), original.credential())?,
                enrollment: *operation,
                reservation: None,
            };
            receiving
                .pairing
                .prepare_route(&prepared, original_binding.target().route().clone())
                .await
                .map_err(classify_pairing)?;
            receiving
                .pairing
                .save_additional_client(&prepared, receipt)
                .await
                .map_err(classify_pairing)?
        } else {
            return Err(ManagementFailure::UnsafeNativeState);
        };

        let PairingReceiptBody::ClientBinding { operation, .. } = &saved.receipt().parts().claims
        else {
            return Err(ManagementFailure::AuthorizationDenied);
        };
        // A receipt retained before an interrupted acknowledgement keeps the
        // original stream. Replay settlement, never the binding operation.
        operations
            .tracker
            .acknowledge(operation.id.stream(), operations.as_ref())
            .await
            .map_err(classify_client)?;
        wait_acknowledgement(owner.pairing.journal(), &saved).await?;

        receiving
            .pairing
            .confirm_additional_client_acknowledgement(
                saved.key(),
                saved.client().parts().client.id,
                owner.pairing.journal(),
            )
            .await
            .map_err(classify_pairing)?;
        receiving
            .pairing
            .restore(saved.key(), saved.client().parts().client.id)
            .await
            .map_err(classify_pairing)?;
        if name == "cli" {
            cli = Some(saved.client().parts().client.id);
        }
    }
    cli.ok_or(ManagementFailure::PairingRequired)
}

fn route_reference(
    journal: &JournalController,
    credential: &PreparedCredential,
) -> Result<RevocationRouteReference, ManagementFailure> {
    journal
        .snapshot()
        .map_err(classify_client)?
        .pairing()
        .slot(credential.scope.reference())
        .and_then(|slot| slot.route_reference)
        .ok_or(ManagementFailure::UnsafeNativeState)
}

async fn wait_acknowledgement(
    journal: &JournalController,
    saved: &spaceterm_client::pairing::journal::SavedPairing,
) -> Result<(), ManagementFailure> {
    let PairingReceiptBody::ClientBinding { operation, .. } = &saved.receipt().parts().claims
    else {
        return Err(ManagementFailure::AuthorizationDenied);
    };
    let mut changes = journal.revisions();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let state = journal.snapshot().map_err(classify_client)?;
            if state
                .stream(operation.id.stream())
                .is_some_and(|header| header.confirmed_prefix >= operation.id.sequence().get())
            {
                return Ok(());
            }
            changes
                .changed()
                .await
                .map_err(|_| ManagementFailure::StorageUnavailable)?;
        }
    })
    .await
    .map_err(|_| ManagementFailure::OutcomePending)?
}

async fn open_journal(
    admission: &ClientProvisioning,
    consumer: &str,
) -> Result<Arc<JournalController>, ManagementFailure> {
    let (directory, create) = admission
        .journal_directory(consumer)
        .map_err(classify_service)?;
    let codec = Arc::new(CanonicalCodec);
    let verifier = Arc::new(PinnedReceipts);
    let limits = JournalLimits::default();
    let storage = if create {
        NativeClientJournal::create(directory, codec.clone(), verifier.clone(), limits).await
    } else {
        NativeClientJournal::open_existing(directory, codec.clone(), verifier.clone(), limits).await
    }
    .map_err(classify_client)?;
    JournalController::recover(Arc::new(storage), codec, verifier, limits)
        .await
        .map(Arc::new)
        .map_err(classify_client)
}

async fn enroll_owner(
    native: &NativeServiceManagement,
    registration: &RegisteredServer,
    pairing: &NativePairingRuntime,
    route: spaceterm_client::api::RouteDescription,
) -> Result<(), ManagementFailure> {
    let server = ServerId::new(registration.server_pin().identity);
    let namespace = CredentialNamespace::new(format!(
        "spaceterm-native-{}",
        match registration.key().build_identity {
            BuildIdentity::Release => "release",
            BuildIdentity::Preflight => "preflight",
            BuildIdentity::Development => "development",
        }
    ))
    .map_err(classify_pairing)?;
    let (_, device) = pairing
        .prepare_credential(
            DeviceCredentials::new_scope(namespace.clone(), CredentialPurpose::DeviceIdentity)
                .map_err(classify_pairing)?,
        )
        .await
        .map_err(classify_pairing)?;
    let (generation, authentication) = pairing
        .prepare_credential(
            DeviceCredentials::new_scope(
                namespace,
                CredentialPurpose::PairingAuthentication { server },
            )
            .map_err(classify_pairing)?,
        )
        .await
        .map_err(classify_pairing)?;
    let reference = RevocationRouteReference::new(random()?);
    let attempt =
        DirectEnrollmentAttempt::new(registration.server_pin(), reference, generation.get())
            .map_err(classify_pairing)?;
    let verified = attempt
        .confirm(
            registration.server_pin(),
            reference,
            generation.get(),
            &attempt.fingerprint(),
        )
        .map_err(classify_pairing)?;
    let credentials = pairing.credentials();
    let device_key = credentials
        .signing_key(
            &device,
            &verified.credential_use(&device).map_err(classify_pairing)?,
        )
        .await
        .map_err(classify_pairing)?;
    let auth_key = credentials
        .signing_key(
            &authentication,
            &verified
                .credential_use(&authentication)
                .map_err(classify_pairing)?,
        )
        .await
        .map_err(classify_pairing)?;
    let proposal = EnrollmentProposal::new(EnrollmentProposalParts {
        server,
        device: DeviceId::new(random()?),
        keys: PairingKeys::new(PairingKeysParts {
            device_identity_public_key: device.public_key,
            pairing_authentication_public_key: authentication.public_key,
        })
        .map_err(|_| ManagementFailure::UnsafeNativeState)?,
        claim_nonce: random()?,
    })
    .map_err(|_| ManagementFailure::UnsafeNativeState)?;
    let (device_proof, authentication_proof) =
        VerifiedDeviceClaim::proofs(&proposal, device_key.as_ref(), auth_key.as_ref())
            .map_err(|_| ManagementFailure::CredentialUnavailable)?;
    let claim =
        enrollment_proposal_digest(&proposal).map_err(|_| ManagementFailure::UnsafeNativeState)?;
    let request = native
        .supervisor
        .allocate_request()
        .map_err(classify_service)?;
    let digest = native
        .supervisor
        .owner_enrollment_digest(request, server, claim)
        .map_err(classify_service)?;
    native
        .supervisor
        .record_owner_enrollment(request, digest, server, claim)
        .map_err(classify_service)?;
    let ordinary = registration.endpoint().map_err(classify_service)?;
    let endpoint = PrivateIpcDirectory::open(
        ordinary
            .path()
            .parent()
            .ok_or(ManagementFailure::UnsafeNativeState)?,
    )
    .and_then(|directory| directory.endpoint("owner"))
    .map_err(|_| ManagementFailure::NativeUnavailable)?;
    let transport = Arc::new(
        IpcTransport::connect(
            &endpoint,
            MessageLimit::new(OWNER_LIMIT).map_err(|_| ManagementFailure::UnsafeNativeState)?,
        )
        .await
        .map_err(|_| ManagementFailure::Offline)?,
    );
    let exchange = tokio::time::timeout(Duration::from_secs(30), async {
        let bytes = serde_json::to_vec(&OwnerRequest { request, proposal: proposal.clone(), device_proof: device_proof.to_vec(), authentication_proof: authentication_proof.to_vec() }).map_err(|_| ManagementFailure::UnsafeNativeState)?;
        transport.send(&bytes).await.map_err(|_| ManagementFailure::Offline)?;
        let bytes = transport.recv().await.map_err(|_| ManagementFailure::Offline)?.ok_or(ManagementFailure::Offline)?;
        let reply: OwnerReply = serde_json::from_slice(&bytes).map_err(|_| ManagementFailure::UnsafeNativeState)?;
        if reply.schema != 1 || reply.disposition != "authorization_delivered" || !matches!(reply.outcome, PairingOutcome::AuthorizationReserved { claim_digest, .. } if claim_digest == claim) { return Err(ManagementFailure::AuthorizationUnavailable); }
        let delivery = transport.recv().await.map_err(|_| ManagementFailure::Offline)?.ok_or(ManagementFailure::Offline)?;
        EnrollmentAuthorization::from_delivery(Zeroizing::new(delivery)).map_err(|_| ManagementFailure::UnsafeNativeState)
    }).await.map_err(|_| ManagementFailure::Offline)?;
    let (reservation, authorization) = match exchange {
        Ok(delivery) => delivery,
        Err(error) => {
            let _ = transport.close().await;
            return Err(error);
        }
    };
    let parts = reservation.parts();
    if parts.claim_digest != claim
        || parts.target.parts().server != server
        || parts.target.parts().device != proposal.parts().device
        || parts.keys != proposal.parts().keys
    {
        let _ = transport.close().await;
        return Err(ManagementFailure::AuthorizationDenied);
    }
    let prepared = PreparedEnrollment {
        local_pairing: LocalPairingKey::new(LocalPairingKeyParts {
            server,
            pairing: parts.target.parts().pairing,
            generation,
        })
        .map_err(|_| ManagementFailure::UnsafeNativeState)?,
        device: parts.target.parts().device,
        pin: registration.server_pin(),
        keys: parts.keys.clone(),
        client: parts.initial_client,
        credential: authentication,
        route: reference,
        enrollment: OperationBinding {
            id: OperationId::new(
                parts.enrollment_stream,
                OperationSequence::new(1).map_err(|_| ManagementFailure::UnsafeNativeState)?,
            ),
            digest: parts.enrollment_request_digest,
        },
        reservation: Some(reservation),
    };
    let local = LocalRouteAuthority::new(registration.endpoint().map_err(classify_service)?)
        .map_err(classify_client)?;
    let restricted = Arc::new(NativePairingConnector::new(
        pairing.journal().clone(),
        NativeRouteConnector::new(vec![local], None).map_err(classify_client)?,
    ));
    let owner_runtime = NativePairingRuntime::with_adapters(
        pairing.journal().clone(),
        pairing.credentials(),
        Arc::new(OwnerConnector {
            pin: registration.server_pin(),
            reference,
            transport: Mutex::new(Some(transport.clone())),
        }),
        restricted,
        Arc::new(ConsumerInvalidator::default()),
        HandshakeConfig::default(),
    );
    let result = owner_runtime
        .enroll_account_owner(
            &native.supervisor,
            OwnerEnrollmentRequest {
                prepared: &prepared,
                device: &device,
                authorization: Some(&authorization),
                native_request: request,
                native_digest: digest,
            },
            route,
        )
        .await
        .map_err(classify_pairing);
    let closed = transport
        .close()
        .await
        .map_err(|_| ManagementFailure::Offline);
    result?;
    closed?;
    if owner_runtime
        .inventory()
        .await
        .map_err(classify_pairing)?
        .enrollment_acknowledgements
        .iter()
        .any(|ack| ack.prepared.enrollment.id == prepared.enrollment.id)
    {
        return Err(ManagementFailure::OutcomePending);
    }
    Ok(())
}
fn random<const N: usize>() -> Result<[u8; N], ManagementFailure> {
    let mut value = [0; N];
    SystemRandom::new()
        .fill(&mut value)
        .map_err(|_| ManagementFailure::NativeUnavailable)?;
    Ok(value)
}

struct OwnerConnector {
    pin: ServerPin,
    reference: RevocationRouteReference,
    transport: Mutex<Option<Arc<IpcTransport>>>,
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
struct OwnerTransport(Arc<IpcTransport>);
impl PairingTransport for OwnerTransport {
    fn send<'a>(&'a mut self, bytes: &'a [u8]) -> PairingFuture<'a, Result<(), PairingFailure>> {
        Box::pin(async move {
            tokio::time::timeout(Duration::from_secs(30), self.0.send(bytes))
                .await
                .map_err(|_| PairingFailure::TimedOut)?
                .map_err(|_| PairingFailure::Offline)
        })
    }
    fn receive(&mut self) -> PairingFuture<'_, Result<Zeroizing<Vec<u8>>, PairingFailure>> {
        Box::pin(async move {
            tokio::time::timeout(Duration::from_secs(30), self.0.recv())
                .await
                .map_err(|_| PairingFailure::TimedOut)?
                .map_err(|_| PairingFailure::Offline)?
                .filter(|bytes| !bytes.is_empty() && bytes.len() <= OWNER_LIMIT)
                .map(Zeroizing::new)
                .ok_or(PairingFailure::Offline)
        })
    }
}
