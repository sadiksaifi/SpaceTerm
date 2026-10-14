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
    native_deployment::contracts::NativeRequestId,
    startup::{BuildIdentity, DesiredServerState},
    transport::MessageTransport,
};
use spaceterm_protocol::pairing_adapter::{
    EnrollmentAuthorization, PublicEnrollmentReservation, VerifiedDeviceClaim,
    enrollment_proposal_digest,
};
use std::time::Duration;
use zeroize::Zeroizing;

mod preparation;
pub(super) mod recovery;

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

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnerAttempt {
    server_public_key: [u8; 32],
    proposal: EnrollmentProposal,
    request: NativeRequestId,
    generation: spaceterm_core::pairing::LocalPairingGeneration,
    device: PreparedCredential,
    authentication: PreparedCredential,
    reference: RevocationRouteReference,
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
        if !registration.is_installed() {
            return Err(ManagementFailure::NativeUnavailable);
        }
        // Deliberate admission recovers the exact original metadata publication
        // before checking recovery intent under its lease. Ordinary startup keeps
        // its read-only guard and cannot repair a shared-directory fence.
        // An active dedicated CLI can issue additional Clients with its own
        // authority. It never borrows a desktop Client or enrolls another device.
        let admission = self
            .native
            .supervisor
            .admit_client_provisioning()
            .map_err(classify_service)?;
        if recover_account {
            return self.recover_account(&registration, &admission).await;
        }
        self.require_account_recovery_complete()?;
        if registration.desired_state() == DesiredServerState::Running
            && admission
                .owner_attempt("enrollment")
                .map_err(classify_service)?
                .is_none()
        {
            let cli_journal = open_journal(&admission, "cli").await?;
            let route =
                LocalRouteAuthority::new(registration.endpoint().map_err(classify_service)?)
                    .map_err(classify_client)?;
            let recovered = pairing_owner(
                cli_journal,
                self.credentials.clone(),
                NativeRouteConnector::new(vec![route], None).map_err(classify_client)?,
            );
            let existing = self.owner.get().unwrap_or(&recovered);
            {
                let inventory = existing
                    .pairing
                    .inventory()
                    .await
                    .map_err(classify_pairing)?;
                if let [saved] = inventory.saved.as_slice() {
                    if saved.pin() != pin {
                        return Err(ManagementFailure::AuthorizationDenied);
                    }
                    existing
                        .pairing
                        .restore(saved.key(), saved.client().parts().client.id)
                        .await
                        .map_err(classify_pairing)?;
                    let client = self
                        .provision_from_owner(&registration, existing, &admission, true)
                        .await?;
                    return Ok(CliOutput::Enrolled {
                        server: ServerId::new(pin.identity),
                        client,
                    });
                }
                if !inventory.revocations.is_empty() {
                    return Err(ManagementFailure::Revoked);
                }
            }
        }
        let journal = open_journal(&admission, "enrollment").await?;
        let route = LocalRouteAuthority::new(registration.endpoint().map_err(classify_service)?)
            .map_err(classify_client)?;
        let description = route.description().clone();
        let owner = pairing_owner(
            journal,
            self.credentials.clone(),
            NativeRouteConnector::new(vec![route], None).map_err(classify_client)?,
        );
        self.enroll_primary(&registration, &owner, &admission, "enrollment", description)
            .await?;
        if registration.desired_state() == DesiredServerState::Stopped {
            let inventory = owner.pairing.inventory().await.map_err(classify_pairing)?;
            let [saved] = inventory.saved.as_slice() else {
                return Err(ManagementFailure::UnsafeNativeState);
            };
            return Ok(CliOutput::Enrolled {
                server: ServerId::new(pin.identity),
                client: saved.client().parts().client.id,
            });
        }
        let client = self
            .provision_from_owner(&registration, &owner, &admission, false)
            .await?;
        Ok(CliOutput::Enrolled {
            server: ServerId::new(pin.identity),
            client,
        })
    }

    /// Deliberate desktop owner enrollment shares the same retained claim and
    /// delivery recovery as CLI enrollment. Ordinary restoration never calls it.
    pub async fn enroll_journal(
        &self,
        journal: Arc<JournalController>,
    ) -> Result<(), ManagementFailure> {
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
        self.require_account_recovery_complete()?;
        let route = LocalRouteAuthority::new(registration.endpoint().map_err(classify_service)?)
            .map_err(classify_client)?;
        let description = route.description().clone();
        let owner = pairing_owner(
            journal,
            self.credentials.clone(),
            NativeRouteConnector::new(vec![route], None).map_err(classify_client)?,
        );
        self.enroll_primary(&registration, &owner, &admission, "desktop", description)
            .await?;
        if registration.desired_state() == DesiredServerState::Running {
            self.provision_from_owner(&registration, &owner, &admission, false)
                .await?;
        }
        Ok(())
    }

    async fn enroll_primary(
        &self,
        registration: &RegisteredServer,
        owner: &PairingOwner,
        admission: &ClientProvisioning,
        consumer: &str,
        route: spaceterm_client::api::RouteDescription,
    ) -> Result<(), ManagementFailure> {
        let inventory = owner.pairing.inventory().await.map_err(classify_pairing)?;
        let selected = inventory
            .saved
            .iter()
            .filter(|saved| saved.pin() == registration.server_pin())
            .count();
        if selected > 1 {
            return Err(ManagementFailure::UnsafeNativeState);
        }
        let pending_ack = inventory
            .enrollment_acknowledgements
            .iter()
            .any(|ack| ack.prepared.pin == registration.server_pin());
        if selected == 0 || pending_ack {
            if inventory.revocations.iter().any(|work| {
                work.task.parts().local_pairing.parts().server.bytes()
                    == registration.server_pin().identity
            }) {
                return Err(ManagementFailure::Revoked);
            }
            self.enroll_owner(registration, &owner.pairing, route, admission, consumer)
                .await?;
        }
        let inventory = owner.pairing.inventory().await.map_err(classify_pairing)?;
        let saved = inventory
            .saved
            .iter()
            .find(|saved| saved.pin() == registration.server_pin())
            .ok_or(ManagementFailure::PairingRequired)?;
        owner
            .pairing
            .restore(saved.key(), saved.client().parts().client.id)
            .await
            .map_err(classify_pairing)?
            .signing_key()
            .await
            .map_err(classify_pairing)?;
        Ok(())
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
        self.require_account_recovery_complete()?;
        let route = LocalRouteAuthority::new(registration.endpoint().map_err(classify_service)?)
            .map_err(classify_client)?;
        let owner = PairingOwner {
            pairing: originating,
            routes: Arc::new(
                NativeRouteConnector::new(vec![route], None).map_err(classify_client)?,
            ),
            invalidator: Arc::new(ConsumerInvalidator::default()),
        };
        self.provision_from_owner(&registration, &owner, &admission, false)
            .await
    }

    async fn provision_from_owner(
        &self,
        registration: &RegisteredServer,
        owner: &PairingOwner,
        admission: &ClientProvisioning,
        cli_origin: bool,
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
        if let Some(cli) = provisioned_clients(admission, owner, original, cli_origin).await? {
            return Ok(cli);
        }
        let opened = open_pairing(
            NativeConsumerKind::Cli,
            owner,
            false,
            Some(registration.server_pin()),
        )
        .await?;
        let result = provision(
            registration,
            owner,
            original,
            &opened,
            admission,
            cli_origin,
        )
        .await;
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

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProvisioningAttempt {
    server_public_key: [u8; 32],
    target: spaceterm_core::pairing::PairingTarget,
    origin: spaceterm_core::identity::OperationStreamId,
    phase: ProvisioningPhase,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
enum ProvisioningPhase {
    Assessment {
        operation: OperationId,
    },
    Binding {
        assessment: OperationId,
        operation: OperationId,
        version: spaceterm_core::authority::FieldVersion,
    },
    Saved {
        operation: OperationId,
    },
    Failure {
        operation: OperationId,
        failure: PairingFailure,
    },
    RetryReady {
        operation: OperationId,
    },
    Complete {
        operation: OperationId,
    },
}
impl ProvisioningPhase {
    fn operation(self) -> OperationId {
        match self {
            Self::Assessment { operation }
            | Self::Binding { operation, .. }
            | Self::Saved { operation }
            | Self::Failure { operation, .. }
            | Self::RetryReady { operation }
            | Self::Complete { operation } => operation,
        }
    }
    fn incomplete(self) -> bool {
        !matches!(self, Self::RetryReady { .. } | Self::Complete { .. })
    }
}

struct RetainedProvisioning {
    attempt: ProvisioningAttempt,
    bytes: Vec<u8>,
}
impl RetainedProvisioning {
    fn read(admission: &ClientProvisioning, name: &str) -> Result<Option<Self>, ManagementFailure> {
        let Some(bytes) = admission
            .provisioning_attempt(name)
            .map_err(classify_service)?
        else {
            return Ok(None);
        };
        let attempt: ProvisioningAttempt =
            serde_json::from_slice(&bytes).map_err(|_| ManagementFailure::UnsafeNativeState)?;
        if attempt.origin.server() != attempt.target.parts().server
            || attempt.origin.pairing() != attempt.target.parts().pairing
            || attempt.phase.operation().stream() != attempt.origin
            || matches!(attempt.phase, ProvisioningPhase::Binding { assessment, operation, .. }
                if assessment.stream() != attempt.origin || assessment.sequence() >= operation.sequence())
        {
            return Err(ManagementFailure::UnsafeNativeState);
        }
        Ok(Some(Self { attempt, bytes }))
    }

    fn publish(
        admission: &ClientProvisioning,
        name: &str,
        expected: Option<&[u8]>,
        attempt: ProvisioningAttempt,
    ) -> Result<Self, ManagementFailure> {
        let bytes =
            serde_json::to_vec(&attempt).map_err(|_| ManagementFailure::UnsafeNativeState)?;
        admission
            .retain_provisioning_attempt(name, expected, &bytes)
            .map_err(classify_service)?;
        Ok(Self { attempt, bytes })
    }

    fn advance(
        &mut self,
        admission: &ClientProvisioning,
        name: &str,
        phase: ProvisioningPhase,
    ) -> Result<(), ManagementFailure> {
        let mut attempt = self.attempt.clone();
        attempt.phase = phase;
        *self = Self::publish(admission, name, Some(&self.bytes), attempt)?;
        Ok(())
    }

    fn matches_origin(
        &self,
        original: &spaceterm_client::pairing::journal::SavedPairing,
        target: &spaceterm_core::pairing::PairingTarget,
    ) -> bool {
        self.attempt.server_public_key == original.pin().public_key
            && &self.attempt.target == target
            && self.attempt.origin == original.client().parts().ordinary
    }
}

async fn provisioned_clients(
    admission: &ClientProvisioning,
    owner: &PairingOwner,
    original: &spaceterm_client::pairing::journal::SavedPairing,
    cli_origin: bool,
) -> Result<Option<spaceterm_core::identity::ClientId>, ManagementFailure> {
    let (original_target, original_keys) = match &original.receipt().parts().claims {
        PairingReceiptBody::Enrollment { target, keys, .. }
        | PairingReceiptBody::ClientBinding { target, keys, .. } => (target, keys),
        _ => return Err(ManagementFailure::AuthorizationDenied),
    };
    // Check all receivers before opening any originating connection or issuing
    // work. A usable CLI does not own interrupted desktop provisioning.
    let mut incomplete = false;
    for name in ["cli", "companion", "management"] {
        if let Some(retained) = RetainedProvisioning::read(admission, name)?
            && retained.attempt.phase.incomplete()
        {
            if !retained.matches_origin(original, original_target) {
                return Err(ManagementFailure::OutcomePending);
            }
            incomplete = true;
        }
    }
    if incomplete {
        return Ok(None);
    }
    let mut cli = None;
    let mut clients = std::collections::BTreeSet::new();
    for name in ["cli", "companion", "management"] {
        let journal = if name == "cli" && cli_origin {
            owner.pairing.journal().clone()
        } else {
            open_journal(admission, name).await?
        };
        let state = journal.snapshot().map_err(classify_client)?;
        let saved = match state.bindings() {
            [] => return Ok(None),
            [saved] => saved,
            _ => return Err(ManagementFailure::UnsafeNativeState),
        };
        let PairingReceiptBody::ClientBinding { target, keys, .. } = &saved.proof().parts().claims
        else {
            return Err(ManagementFailure::AuthorizationDenied);
        };
        let binding = saved.binding().map_err(classify_client)?;
        if binding.target().pin() != original.pin()
            || target != original_target
            || keys != original_keys
            || !clients.insert(binding.target().client())
        {
            return Err(ManagementFailure::AuthorizationDenied);
        }
        if !journal
            .ordinary_use_allowed(&binding)
            .map_err(classify_client)?
        {
            return Ok(None);
        }
        if name == "cli" {
            cli = Some(binding.target().client());
        }
    }
    Ok(cli)
}

async fn provision(
    registration: &RegisteredServer,
    owner: &PairingOwner,
    original: &spaceterm_client::pairing::journal::SavedPairing,
    opened: &OpenedConsumer,
    admission: &ClientProvisioning,
    cli_origin: bool,
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
        let mut retained = RetainedProvisioning::read(admission, name)?;
        if let Some(attempt) = &retained
            && attempt.attempt.phase.incomplete()
            && !attempt.matches_origin(original, target)
        {
            return Err(ManagementFailure::OutcomePending);
        }
        let journal = if name == "cli" && cli_origin {
            owner.pairing.journal().clone()
        } else {
            open_journal(admission, name).await?
        };
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
                || !matches!(&saved.receipt().parts().claims,
                    PairingReceiptBody::ClientBinding { target: candidate, keys: candidate_keys, .. }
                        if candidate == target && candidate_keys == keys)
            {
                return Err(ManagementFailure::AuthorizationDenied);
            }
            saved.clone()
        } else if inventory.saved.is_empty() {
            if inventory.revocations.iter().any(|work| {
                work.task.parts().local_pairing.parts().pairing == original.key().parts().pairing
                    && work.task.parts().local_pairing.parts().server
                        == original.key().parts().server
            }) {
                return Err(ManagementFailure::Revoked);
            }
            if retained
                .as_ref()
                .is_none_or(|value| !value.attempt.phase.incomplete())
            {
                // An older binding without receiver metadata cannot be assigned
                // to a receiver by scanning whichever stream is now available.
                refuse_unowned_binding(owner.pairing.journal(), original, target)?;
                let operation = reserve_provisioning_operation(
                    owner.pairing.journal(),
                    &original_binding,
                    target,
                    None,
                    operations,
                )
                .await?;
                retained = Some(RetainedProvisioning::publish(
                    admission,
                    name,
                    retained.as_ref().map(|value| value.bytes.as_slice()),
                    ProvisioningAttempt {
                        server_public_key: original.pin().public_key,
                        target: target.clone(),
                        origin: original.client().parts().ordinary,
                        phase: ProvisioningPhase::Assessment { operation },
                    },
                )?);
            }
            let attempt = retained
                .as_mut()
                .ok_or(ManagementFailure::UnsafeNativeState)?;
            if let ProvisioningPhase::Failure { .. } = attempt.attempt.phase {
                return Err(settle_provisioning_failure(
                    admission,
                    name,
                    attempt,
                    owner.pairing.journal(),
                    operations,
                )
                .await?);
            }
            if let ProvisioningPhase::Assessment { operation } = attempt.attempt.phase {
                let outcome =
                    recover_provisioning_assessments(owner, operations, operation, target).await?;
                if let Outcome::Pairing(value) = &outcome
                    && let PairingOutcome::Failed(failure) = value.as_ref()
                {
                    attempt.advance(
                        admission,
                        name,
                        ProvisioningPhase::Failure {
                            operation,
                            failure: *failure,
                        },
                    )?;
                    return Err(settle_provisioning_failure(
                        admission,
                        name,
                        attempt,
                        owner.pairing.journal(),
                        operations,
                    )
                    .await?);
                }
                let version = assessment_version(outcome, target)?;
                let receiver = provisioning_receiver(name)?;
                let binding = match recover_receiver_binding(
                    owner.pairing.journal(),
                    original,
                    target,
                    receiver,
                    operation,
                    version,
                )? {
                    Some(binding) => binding,
                    None => {
                        refuse_unowned_binding(owner.pairing.journal(), original, target)?;
                        reserve_provisioning_operation(
                            owner.pairing.journal(),
                            &original_binding,
                            target,
                            Some((receiver, operation, version)),
                            operations,
                        )
                        .await?
                    }
                };
                // Retain the successful assessment and durably reserved binding
                // before consumption can permit pruning the assessment result.
                attempt.advance(
                    admission,
                    name,
                    ProvisioningPhase::Binding {
                        assessment: operation,
                        operation: binding,
                        version,
                    },
                )?;
            }
            let ProvisioningPhase::Binding {
                assessment,
                operation,
                version,
            } = attempt.attempt.phase
            else {
                return Err(ManagementFailure::OutcomePending);
            };
            consume_result(owner.pairing.journal(), assessment).await?;
            let proof = owner
                .pairing
                .prepare_additional_client(
                    original.key(),
                    original.client().parts().client.id,
                    version,
                    operation,
                )
                .await
                .map_err(classify_pairing)?;
            let state = owner
                .pairing
                .journal()
                .snapshot()
                .map_err(classify_client)?;
            let outcome = match state.operation(operation) {
                Some(record) => {
                    let request = spaceterm_core::operation_ledger_contracts::CanonicalOperationRequest::decode(&record.request, &CanonicalCodec)
                        .map_err(|_| ManagementFailure::UnsafeNativeState)?;
                    if request.command() != &Command::Pairing(proof.command().clone()) {
                        return Err(ManagementFailure::AuthorizationDenied);
                    }
                    match &record.state {
                        spaceterm_client::journal::RetainedOperationState::Completed {
                            outcome,
                            ..
                        } => outcome
                            .outcome(&CanonicalCodec)
                            .map_err(|_| ManagementFailure::UnsafeNativeState)?,
                        spaceterm_client::journal::RetainedOperationState::Pending => {
                            let ticket = operations
                                .resolve_additional_client(proof)
                                .await
                                .map_err(classify_client)?;
                            operations.wait(&ticket).await.map_err(classify_client)?
                        }
                        _ => return Err(ManagementFailure::UnsafeNativeState),
                    }
                }
                None => return Err(ManagementFailure::OutcomePending),
            };
            if let Outcome::Pairing(value) = &outcome
                && let PairingOutcome::Failed(failure) = value.as_ref()
            {
                attempt.advance(
                    admission,
                    name,
                    ProvisioningPhase::Failure {
                        operation,
                        failure: *failure,
                    },
                )?;
                return Err(settle_provisioning_failure(
                    admission,
                    name,
                    attempt,
                    owner.pairing.journal(),
                    operations,
                )
                .await?);
            }
            let receipt = additional_receipt(outcome)?;
            let PairingReceiptBody::ClientBinding {
                client, operation, ..
            } = &receipt.parts().claims
            else {
                return Err(ManagementFailure::UnsafeNativeState);
            };
            let originating_inventory =
                owner.pairing.inventory().await.map_err(classify_pairing)?;
            let device = originating_inventory
                .credential_preparations
                .iter()
                .filter_map(|value| value.credential.as_ref())
                .find(|credential| {
                    credential.scope.parts().purpose == CredentialPurpose::DeviceIdentity
                        && credential.public_key == keys.parts().device_identity_public_key
                })
                .ok_or(ManagementFailure::CredentialUnavailable)?;
            receiving
                .pairing
                .retain_existing_credential(device.scope.clone(), device.public_key)
                .await
                .map_err(classify_pairing)?;
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
        if let Some(attempt) = &retained
            && attempt.attempt.phase.incomplete()
            && (!matches!(
                attempt.attempt.phase,
                ProvisioningPhase::Binding { .. } | ProvisioningPhase::Saved { .. }
            ) || attempt.attempt.phase.operation() != operation.id)
        {
            return Err(ManagementFailure::UnsafeNativeState);
        }
        let saved_binding = saved_binding(&receiving.pairing, &saved)?;
        if receiving
            .pairing
            .journal()
            .ordinary_use_allowed(&saved_binding)
            .map_err(classify_client)?
        {
            // Already usable Clients need no access to another origin's stream.
            if operation.id.stream() == original.client().parts().ordinary {
                consume_result(owner.pairing.journal(), operation.id).await?;
                if let Some(attempt) = &mut retained
                    && attempt.attempt.phase.incomplete()
                {
                    attempt.advance(
                        admission,
                        name,
                        ProvisioningPhase::Complete {
                            operation: operation.id,
                        },
                    )?;
                }
            }
            if name == "cli" {
                cli = Some(saved.client().parts().client.id);
            }
            continue;
        }
        if operation.id.stream() != original.client().parts().ordinary {
            return Err(ManagementFailure::OutcomePending);
        }
        // The saved receipt remains recovery authority for an older journal that
        // predates receiver metadata. Retain its exact origin before settlement.
        if let Some(attempt) = &mut retained {
            attempt.advance(
                admission,
                name,
                ProvisioningPhase::Saved {
                    operation: operation.id,
                },
            )?;
        } else {
            retained = Some(RetainedProvisioning::publish(
                admission,
                name,
                None,
                ProvisioningAttempt {
                    server_public_key: original.pin().public_key,
                    target: target.clone(),
                    origin: original.client().parts().ordinary,
                    phase: ProvisioningPhase::Saved {
                        operation: operation.id,
                    },
                },
            )?);
        }
        consume_result(owner.pairing.journal(), operation.id).await?;
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
        retained
            .as_mut()
            .ok_or(ManagementFailure::UnsafeNativeState)?
            .advance(
                admission,
                name,
                ProvisioningPhase::Complete {
                    operation: operation.id,
                },
            )?;
        if name == "cli" {
            cli = Some(saved.client().parts().client.id);
        }
    }
    cli.ok_or(ManagementFailure::PairingRequired)
}

async fn reserve_provisioning_operation(
    journal: &Arc<JournalController>,
    binding: &PairedServerBinding,
    target: &spaceterm_core::pairing::PairingTarget,
    receiver_binding: Option<(
        spaceterm_client::journal::ProvisioningReceiver,
        OperationId,
        spaceterm_core::authority::FieldVersion,
    )>,
    operations: &super::super::tracked::ManagementOperations,
) -> Result<OperationId, ManagementFailure> {
    let state = journal.snapshot().map_err(classify_client)?;
    let stream = state
        .bindings()
        .iter()
        .find(|saved| saved.binding().as_ref() == Ok(binding))
        .ok_or(ManagementFailure::AuthorizationDenied)?
        .streams()
        .parts()
        .ordinary;
    let predecessors = state
        .operations()
        .iter()
        .filter(|record| {
            record.binding.id.stream() == stream
                && matches!(
                    record.state,
                    spaceterm_client::journal::RetainedOperationState::Pending
                )
        })
        .map(|record| record.binding.id)
        .collect::<Vec<_>>();
    // Portable reconciliation settles earlier retained intents without UI
    // consumption. This includes a prepare committed before receiver metadata.
    operations
        .tracker
        .reconcile(binding, operations)
        .await
        .map_err(classify_client)?;
    for predecessor in predecessors {
        let ticket = operations
            .tracker
            .ticket(predecessor)
            .map_err(classify_client)?;
        operations.wait(&ticket).await.map_err(classify_client)?;
    }
    let controller = journal.clone();
    let binding = binding.clone();
    let target = target.clone();
    // The journal serial owner commits the actual ID before receiver metadata
    // can claim it. A crash before this commit cannot claim a later user command.
    journal
        .update(move |state| {
            let invalid = || ClientFailure::new(ClientFailureClass::InvalidStorage);
            if !controller.ordinary_use_allowed(&binding)? || !state.ordinary_use_allowed(&binding)
            {
                return Err(ClientFailure::new(ClientFailureClass::AuthorizationDenied));
            }
            let stream = state
                .bindings()
                .iter()
                .find(|saved| saved.binding().as_ref() == Ok(&binding))
                .ok_or_else(invalid)?
                .streams()
                .parts()
                .ordinary;
            let header = state.stream(stream).ok_or_else(invalid)?;
            if header.retired {
                return Err(ClientFailure::new(ClientFailureClass::StaleAuthority));
            }
            let operation = OperationId::new(
                stream,
                OperationSequence::new(header.high_water.checked_add(1).ok_or_else(invalid)?)
                    .map_err(|_| invalid())?,
            );
            let command = match receiver_binding {
                None => PairingCommand::AssessUnpair {
                    target: target.clone(),
                },
                Some((_, _, version)) => {
                    let (claim_digest, _) =
                        spaceterm_protocol::pairing_adapter::client_binding_claim(
                            &target, version, operation,
                        )
                        .map_err(|_| invalid())?;
                    PairingCommand::BindAdditionalClient {
                        target: target.clone(),
                        expected_authorization_version: version,
                        claim_digest,
                    }
                }
            };
            let request =
                spaceterm_core::operation_ledger_contracts::CanonicalOperationRequest::new(
                    operation,
                    Command::Pairing(command),
                    &CanonicalCodec,
                )
                .map_err(|_| invalid())?;
            let record = spaceterm_client::journal::JournalOperation {
                binding: request.binding(),
                request: request.encode(&CanonicalCodec).map_err(|_| invalid())?,
                workspace: None,
                reserves_grant: false,
                cleanup: false,
                state: spaceterm_client::journal::RetainedOperationState::Pending,
            };
            let change = match receiver_binding {
                None => spaceterm_client::journal::ClientJournalChange::Prepare(record),
                Some((receiver, assessment, version)) => {
                    spaceterm_client::journal::ClientJournalChange::PrepareReceiverBinding {
                        receiver: Box::new(spaceterm_client::journal::JournalReceiverBinding {
                            receiver,
                            operation: record.binding,
                            server_public_key: binding.target().pin().public_key,
                            target,
                            assessment,
                            version,
                        }),
                        operation: record,
                    }
                }
            };
            Ok((vec![change], operation))
        })
        .await
        .map(|(_, operation)| operation)
        .map_err(classify_client)
}

fn provisioning_receiver(
    name: &str,
) -> Result<spaceterm_client::journal::ProvisioningReceiver, ManagementFailure> {
    use spaceterm_client::journal::ProvisioningReceiver;
    match name {
        "cli" => Ok(ProvisioningReceiver::Cli),
        "companion" => Ok(ProvisioningReceiver::Companion),
        "management" => Ok(ProvisioningReceiver::Management),
        _ => Err(ManagementFailure::UnsafeNativeState),
    }
}

fn recover_receiver_binding(
    journal: &JournalController,
    original: &spaceterm_client::pairing::journal::SavedPairing,
    target: &spaceterm_core::pairing::PairingTarget,
    receiver: spaceterm_client::journal::ProvisioningReceiver,
    assessment: OperationId,
    version: spaceterm_core::authority::FieldVersion,
) -> Result<Option<OperationId>, ManagementFailure> {
    let state = journal.snapshot().map_err(classify_client)?;
    let mut owned = state.receiver_bindings().iter().filter(|record| {
        record.receiver == receiver
            && record.server_public_key == original.pin().public_key
            && &record.target == target
            && record.operation.id.stream() == original.client().parts().ordinary
            && record.assessment == assessment
            && record.version == version
    });
    let operation = owned.next().map(|record| record.operation.id);
    if owned.next().is_some() {
        return Err(ManagementFailure::UnsafeNativeState);
    }
    Ok(operation)
}

fn refuse_unowned_binding(
    journal: &JournalController,
    original: &spaceterm_client::pairing::journal::SavedPairing,
    target: &spaceterm_core::pairing::PairingTarget,
) -> Result<(), ManagementFailure> {
    let state = journal.snapshot().map_err(classify_client)?;
    for record in state.operations() {
        if record.binding.id.stream() != original.client().parts().ordinary
            || matches!(
                record.state,
                spaceterm_client::journal::RetainedOperationState::Completed { consumed: true, .. }
                    | spaceterm_client::journal::RetainedOperationState::Retired
            )
        {
            continue;
        }
        let request =
            spaceterm_core::operation_ledger_contracts::CanonicalOperationRequest::decode(
                &record.request,
                &CanonicalCodec,
            )
            .map_err(|_| ManagementFailure::UnsafeNativeState)?;
        if matches!(request.command(), Command::Pairing(PairingCommand::BindAdditionalClient { target: candidate, .. }) if candidate == target)
        {
            return Err(ManagementFailure::OutcomePending);
        }
    }
    Ok(())
}

async fn recover_provisioning_assessments(
    owner: &PairingOwner,
    operations: &super::super::tracked::ManagementOperations,
    operation: OperationId,
    target: &spaceterm_core::pairing::PairingTarget,
) -> Result<Outcome, ManagementFailure> {
    let state = owner
        .pairing
        .journal()
        .snapshot()
        .map_err(classify_client)?;
    match state.operation(operation) {
        Some(record) => {
            let request =
                spaceterm_core::operation_ledger_contracts::CanonicalOperationRequest::decode(
                    &record.request,
                    &CanonicalCodec,
                )
                .map_err(|_| ManagementFailure::UnsafeNativeState)?;
            if !matches!(request.command(), Command::Pairing(PairingCommand::AssessUnpair { target: candidate }) if candidate == target)
            {
                return Err(ManagementFailure::AuthorizationDenied);
            }
            match &record.state {
                spaceterm_client::journal::RetainedOperationState::Completed {
                    outcome, ..
                } => outcome
                    .outcome(&CanonicalCodec)
                    .map_err(|_| ManagementFailure::UnsafeNativeState),
                spaceterm_client::journal::RetainedOperationState::Pending => {
                    let ticket = operations
                        .resolve_original(operation)
                        .await
                        .map_err(classify_client)?;
                    operations.wait(&ticket).await.map_err(classify_client)
                }
                _ => Err(ManagementFailure::UnsafeNativeState),
            }
        }
        None => Err(ManagementFailure::OutcomePending),
    }
}

async fn settle_provisioning_failure(
    admission: &ClientProvisioning,
    name: &str,
    retained: &mut RetainedProvisioning,
    journal: &JournalController,
    operations: &super::super::tracked::ManagementOperations,
) -> Result<ManagementFailure, ManagementFailure> {
    let ProvisioningPhase::Failure { operation, failure } = retained.attempt.phase else {
        return Err(ManagementFailure::UnsafeNativeState);
    };
    consume_result(journal, operation).await?;
    operations
        .tracker
        .acknowledge(operation.stream(), operations)
        .await
        .map_err(classify_client)?;
    wait_operation_acknowledgement(journal, operation).await?;
    retained.advance(admission, name, ProvisioningPhase::RetryReady { operation })?;
    Ok(classify_pairing(failure))
}

fn assessment_version(
    outcome: Outcome,
    target: &spaceterm_core::pairing::PairingTarget,
) -> Result<spaceterm_core::authority::FieldVersion, ManagementFailure> {
    match outcome {
        Outcome::Pairing(value) => match *value {
            PairingOutcome::UnpairAssessed(value) if &value.parts().target == target => {
                Ok(value.parts().authorization_version)
            }
            PairingOutcome::Failed(error) => Err(classify_pairing(error)),
            _ => Err(ManagementFailure::UnsafeNativeState),
        },
        _ => Err(ManagementFailure::UnsafeNativeState),
    }
}

fn saved_binding(
    pairing: &NativePairingRuntime,
    saved: &spaceterm_client::pairing::journal::SavedPairing,
) -> Result<PairedServerBinding, ManagementFailure> {
    pairing
        .journal()
        .snapshot()
        .map_err(classify_client)?
        .bindings()
        .iter()
        .find(|binding| binding.proof() == saved.receipt())
        .ok_or(ManagementFailure::UnsafeNativeState)?
        .binding()
        .map_err(classify_client)
}

fn additional_receipt(
    outcome: Outcome,
) -> Result<spaceterm_core::pairing::SignedPairingReceipt, ManagementFailure> {
    match outcome {
        Outcome::Pairing(value) => match *value {
            PairingOutcome::ClientBound(receipt) => Ok(receipt),
            PairingOutcome::Failed(error) => Err(classify_pairing(error)),
            _ => Err(ManagementFailure::UnsafeNativeState),
        },
        _ => Err(ManagementFailure::UnsafeNativeState),
    }
}
async fn consume_result(
    journal: &JournalController,
    operation: OperationId,
) -> Result<(), ManagementFailure> {
    let state = journal.snapshot().map_err(classify_client)?;
    if let Some(record) = state.operation(operation) {
        if !matches!(
            record.state,
            spaceterm_client::journal::RetainedOperationState::Completed { .. }
        ) {
            return Err(ManagementFailure::OutcomePending);
        }
        journal
            .transact(vec![
                spaceterm_client::journal::ClientJournalChange::ConsumeResult(operation),
            ])
            .await
            .map_err(classify_client)?;
    }
    Ok(())
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
    wait_operation_acknowledgement(journal, operation.id).await
}

async fn wait_operation_acknowledgement(
    journal: &JournalController,
    operation: OperationId,
) -> Result<(), ManagementFailure> {
    let mut changes = journal.revisions();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let state = journal.snapshot().map_err(classify_client)?;
            if state
                .stream(operation.stream())
                .is_some_and(|header| header.confirmed_prefix >= operation.sequence().get())
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
    let (directory, _) = admission
        .journal_directory(consumer)
        .map_err(classify_service)?;
    let codec = Arc::new(CanonicalCodec);
    let verifier = Arc::new(PinnedReceipts);
    let limits = JournalLimits::default();
    let storage =
        NativeClientJournal::provision(directory, codec.clone(), verifier.clone(), limits)
            .await
            .map_err(classify_client)?;
    JournalController::recover(Arc::new(storage), codec, verifier, limits)
        .await
        .map(Arc::new)
        .map_err(classify_client)
}

impl NativeConsumer {
    async fn enroll_owner(
        &self,
        registration: &RegisteredServer,
        pairing: &NativePairingRuntime,
        route: spaceterm_client::api::RouteDescription,
        admission: &ClientProvisioning,
        consumer: &str,
    ) -> Result<(), ManagementFailure> {
        let native = &self.native;
        let channels = self.owner_channels.as_ref();
        let handshake = self.owner_handshake.clone();
        let server = ServerId::new(registration.server_pin().identity);
        let retained = admission
            .owner_attempt(consumer)
            .map_err(classify_service)?;
        let plan = if let Some(bytes) = retained.as_deref() {
            let plan: OwnerAttempt =
                serde_json::from_slice(bytes).map_err(|_| ManagementFailure::UnsafeNativeState)?;
            let inventory = pairing.inventory().await.map_err(classify_pairing)?;
            if plan.server_public_key != registration.server_pin().public_key
                || plan.proposal.parts().server != server
                || plan
                    .proposal
                    .parts()
                    .keys
                    .parts()
                    .device_identity_public_key
                    != plan.device.public_key
                || plan
                    .proposal
                    .parts()
                    .keys
                    .parts()
                    .pairing_authentication_public_key
                    != plan.authentication.public_key
                || !(inventory.credential_preparations.iter().any(|value| {
                    value.generation == plan.generation
                        && value.credential.as_ref() == Some(&plan.authentication)
                }) || inventory.prepared.iter().any(|value| {
                    value.local_pairing.parts().generation == plan.generation
                        && value.credential == plan.authentication
                }) || inventory.saved.iter().any(|value| {
                    value.key().parts().generation == plan.generation
                        && value.credential() == &plan.authentication
                }) || inventory.enrollment_acknowledgements.iter().any(|value| {
                    value.prepared.local_pairing.parts().generation == plan.generation
                        && value.prepared.credential == plan.authentication
                }))
                || !inventory
                    .credential_preparations
                    .iter()
                    .any(|value| value.credential.as_ref() == Some(&plan.device))
            {
                return Err(ManagementFailure::AuthorizationDenied);
            }
            plan
        } else {
            let plan = preparation::retained_preparation(
                native,
                registration,
                pairing,
                admission,
                consumer,
            )
            .await?;
            let bytes =
                serde_json::to_vec(&plan).map_err(|_| ManagementFailure::UnsafeNativeState)?;
            admission
                .retain_owner_attempt(consumer, None, &bytes)
                .map_err(classify_service)?;
            plan
        };
        let OwnerAttempt {
            server_public_key: _,
            proposal,
            request,
            generation,
            device,
            authentication,
            reference,
        } = plan;
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
        let (device_proof, authentication_proof) =
            VerifiedDeviceClaim::proofs(&proposal, device_key.as_ref(), auth_key.as_ref())
                .map_err(|_| ManagementFailure::CredentialUnavailable)?;
        let claim = enrollment_proposal_digest(&proposal)
            .map_err(|_| ManagementFailure::UnsafeNativeState)?;
        let digest = match native
            .supervisor
            .retained_owner_enrollment_digest(request, server, claim)
            .map_err(classify_service)?
        {
            Some(digest) => digest,
            None => {
                let digest = native
                    .supervisor
                    .owner_enrollment_digest(request, server, claim)
                    .map_err(classify_service)?;
                native
                    .supervisor
                    .record_owner_enrollment(request, digest, server, claim)
                    .map_err(classify_service)?;
                digest
            }
        };
        let transport = channels.open(registration).await.map_err(classify_client)?;
        let exchange = tokio::time::timeout(Duration::from_secs(30), async {
        let bytes = serde_json::to_vec(&OwnerRequest { request, proposal: proposal.clone(), device_proof: device_proof.to_vec(), authentication_proof: authentication_proof.to_vec() }).map_err(|_| ManagementFailure::UnsafeNativeState)?;
        transport.send(&bytes).await.map_err(|_| ManagementFailure::Offline)?;
        let bytes = transport.recv().await.map_err(|_| ManagementFailure::Offline)?.ok_or(ManagementFailure::Offline)?;
        let reply: OwnerReply = serde_json::from_slice(&bytes).map_err(|_| ManagementFailure::UnsafeNativeState)?;
        if reply.schema != 1 || !matches!(reply.outcome, PairingOutcome::AuthorizationReserved { claim_digest, .. } if claim_digest == claim) { return Err(ManagementFailure::AuthorizationDenied); }
        let delivery = transport.recv().await.map_err(|_| ManagementFailure::Offline)?.ok_or(ManagementFailure::Offline)?;
        match reply.disposition.as_str() {
            "authorization_delivered" => EnrollmentAuthorization::from_delivery(Zeroizing::new(delivery)).map(|(reservation, authorization)| (reservation, Some(authorization))).map_err(|_| ManagementFailure::UnsafeNativeState),
            "receipt_available" => PublicEnrollmentReservation::decode(&delivery).map(|reservation| (reservation, None)).map_err(|_| ManagementFailure::UnsafeNativeState),
            "authorization_reply_lost" | "authorization_invalidated" => Err(ManagementFailure::OutcomePending),
            _ => Err(ManagementFailure::AuthorizationDenied),
        }
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
            handshake,
        );
        if owner_runtime
            .inventory()
            .await
            .map_err(classify_pairing)?
            .enrollment_acknowledgements
            .iter()
            .any(|ack| ack.prepared.enrollment.id == prepared.enrollment.id)
        {
            let result = owner_runtime
                .acknowledge_enrollment_once(prepared.enrollment.id)
                .await
                .map_err(classify_pairing);
            if let Err(error) = result {
                let _ = transport.close().await;
                return Err(error);
            }
        }
        let result = owner_runtime
            .enroll_account_owner(
                &native.supervisor,
                OwnerEnrollmentRequest {
                    prepared: &prepared,
                    device: &device,
                    authorization: authorization.as_ref(),
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
    transport: Mutex<Option<Arc<dyn MessageTransport>>>,
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
