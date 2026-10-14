//! Explicit account recovery retains intent before suppressing any old binding.
//! The marker and every journal mutation are resumable; no native key is deleted.

use super::preparation::OwnerPreparation;
use super::*;
use spaceterm_core::identity::ClientId;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecoveryBinding {
    consumer: String,
    key: LocalPairingKey,
    client: ClientId,
    credential: PreparedCredential,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AccountRecovery {
    schema: u16,
    server: ServerId,
    server_public_key: [u8; 32],
    previous_attempt: Option<Vec<u8>>,
    bindings: Vec<RecoveryBinding>,
    preparation: OwnerPreparation,
    complete: bool,
}

fn decode(bytes: &[u8]) -> Result<AccountRecovery, ManagementFailure> {
    let recovery: AccountRecovery =
        serde_json::from_slice(bytes).map_err(|_| ManagementFailure::UnsafeNativeState)?;
    if recovery.schema != 1 || recovery.bindings.len() > 4 {
        return Err(ManagementFailure::UnsafeNativeState);
    }
    Ok(recovery)
}

impl NativeConsumer {
    pub(in crate::native::consumer) fn require_account_recovery_complete(
        &self,
    ) -> Result<(), ManagementFailure> {
        require_complete(&self.native.supervisor)
    }

    pub(super) async fn recover_account(
        &self,
        registration: &RegisteredServer,
        admission: &ClientProvisioning,
    ) -> Result<CliOutput, ManagementFailure> {
        let pin = registration.server_pin();
        let server = ServerId::new(pin.identity);
        let route = LocalRouteAuthority::new(registration.endpoint().map_err(classify_service)?)
            .map_err(classify_client)?;
        let description = route.description().clone();
        let owner = pairing_owner(
            open_journal(admission, "enrollment").await?,
            self.credentials.clone(),
            NativeRouteConnector::new(vec![route], None).map_err(classify_client)?,
        );
        let retained = admission
            .owner_attempt("recovery")
            .map_err(classify_service)?;
        let previous = retained.as_deref().map(decode).transpose()?;
        if let Some(recovery) = &previous {
            if recovery.server != server || recovery.server_public_key != pin.public_key {
                return Err(ManagementFailure::AuthorizationDenied);
            }
            if recovery.complete
                && self
                    .recovery_still_active(registration, &owner, recovery)
                    .await?
            {
                let inventory = owner.pairing.inventory().await.map_err(classify_pairing)?;
                let saved = inventory
                    .saved
                    .iter()
                    .find(|saved| {
                        saved.credential().scope == recovery.preparation.authentication_scope
                    })
                    .ok_or(ManagementFailure::UnsafeNativeState)?;
                let client = if registration.desired_state() == DesiredServerState::Stopped {
                    saved.client().parts().client.id
                } else {
                    self.provision_from_owner(registration, &owner, admission, false)
                        .await?
                };
                return Ok(CliOutput::Enrolled { server, client });
            }
        }
        let mut recovery = match previous {
            Some(recovery) if !recovery.complete => recovery,
            _ => {
                // An unfinished receiver still needs its original stream to
                // settle the retained result. Do not suppress that authority.
                for name in ["cli", "companion", "management"] {
                    if let Some(attempt) = RetainedProvisioning::read(admission, name)?
                        && attempt.attempt.phase.incomplete()
                    {
                        return Err(ManagementFailure::OutcomePending);
                    }
                }
                for (owner, preparation) in [
                    ("enrollment", "enrollment-preparation"),
                    ("desktop", "desktop-preparation"),
                ] {
                    if admission
                        .owner_attempt(owner)
                        .map_err(classify_service)?
                        .is_none()
                        && admission
                            .owner_attempt(preparation)
                            .map_err(classify_service)?
                            .is_some()
                    {
                        // The original key effects can still finish. Explicit
                        // recovery cannot abandon either unissued owner attempt.
                        return Err(ManagementFailure::OutcomePending);
                    }
                }
                let mut bindings = Vec::new();
                for consumer in ["enrollment", "cli", "companion", "management"] {
                    let journal = self
                        .recovery_journal(registration, admission, &owner, consumer)
                        .await?;
                    let inventory = journal
                        .pairing
                        .inventory()
                        .await
                        .map_err(classify_pairing)?;
                    if !inventory.prepared.is_empty()
                        || !inventory.enrollment_acknowledgements.is_empty()
                        || !inventory.additional_client_acknowledgements.is_empty()
                    {
                        return Err(ManagementFailure::OutcomePending);
                    }
                    for saved in inventory.saved {
                        if saved.pin() != pin
                            || bindings
                                .iter()
                                .any(|binding: &RecoveryBinding| binding.consumer == consumer)
                        {
                            return Err(ManagementFailure::AuthorizationDenied);
                        }
                        bindings.push(RecoveryBinding {
                            consumer: consumer.to_owned(),
                            key: saved.key().clone(),
                            client: saved.client().parts().client.id,
                            credential: saved.credential().clone(),
                        });
                    }
                }
                let recovery = AccountRecovery {
                    schema: 1,
                    server,
                    server_public_key: pin.public_key,
                    previous_attempt: admission
                        .owner_attempt("enrollment")
                        .map_err(classify_service)?,
                    bindings,
                    preparation: OwnerPreparation::new(registration, &self.native)?,
                    complete: false,
                };
                let bytes = serde_json::to_vec(&recovery)
                    .map_err(|_| ManagementFailure::UnsafeNativeState)?;
                admission
                    .retain_owner_attempt("recovery", retained.as_deref(), &bytes)
                    .map_err(classify_service)?;
                recovery
            }
        };
        // Exact targets were retained before the first journal mutation. A crash
        // between journals leaves ordinary native admission closed by the marker.
        for binding in &recovery.bindings {
            let journal = self
                .recovery_journal(registration, admission, &owner, &binding.consumer)
                .await?;
            let inventory = journal
                .pairing
                .inventory()
                .await
                .map_err(classify_pairing)?;
            if inventory.is_suppressed(&binding.key) {
                continue;
            }
            let saved = inventory
                .saved
                .iter()
                .find(|saved| {
                    saved.key() == &binding.key
                        && saved.client().parts().client.id == binding.client
                        && saved.credential() == &binding.credential
                        && saved.pin() == pin
                })
                .ok_or(ManagementFailure::AuthorizationDenied)?;
            let assessment = journal
                .pairing
                .assess_unpair(saved.key(), binding.client)
                .await
                .map_err(classify_pairing)?;
            journal
                .pairing
                .self_unpair(assessment)
                .await
                .map_err(classify_pairing)?;
        }
        let current = admission
            .owner_attempt("enrollment")
            .map_err(classify_service)?;
        let plan = current
            .as_deref()
            .map(|bytes| {
                serde_json::from_slice::<OwnerAttempt>(bytes)
                    .map_err(|_| ManagementFailure::UnsafeNativeState)
            })
            .transpose()?;
        match plan {
            Some(plan) if plan.request == recovery.preparation.request => {
                if plan.server_public_key != pin.public_key
                    || plan.proposal.parts().server != server
                    || plan.proposal.parts().device != recovery.preparation.device
                    || plan.proposal.parts().claim_nonce != recovery.preparation.claim_nonce
                    || plan.device.scope != recovery.preparation.device_scope
                    || plan.authentication.scope != recovery.preparation.authentication_scope
                    || plan.reference != recovery.preparation.reference
                {
                    return Err(ManagementFailure::AuthorizationDenied);
                }
            }
            _ => {
                if current != recovery.previous_attempt {
                    return Err(ManagementFailure::AuthorizationDenied);
                }
                let plan = recovery
                    .preparation
                    .prepare(registration, &owner.pairing)
                    .await?;
                let bytes =
                    serde_json::to_vec(&plan).map_err(|_| ManagementFailure::UnsafeNativeState)?;
                admission
                    .retain_owner_attempt("enrollment", current.as_deref(), &bytes)
                    .map_err(classify_service)?;
            }
        }
        self.enroll_owner(
            registration,
            &owner.pairing,
            description,
            admission,
            "enrollment",
        )
        .await?;
        let inventory = owner.pairing.inventory().await.map_err(classify_pairing)?;
        let saved = inventory
            .saved
            .iter()
            .find(|saved| {
                saved.credential().scope == recovery.preparation.authentication_scope
                    && saved.pin() == pin
            })
            .ok_or(ManagementFailure::UnsafeNativeState)?;
        if recovery
            .bindings
            .iter()
            .any(|old| old.key.parts().pairing == saved.key().parts().pairing)
        {
            return Err(ManagementFailure::AuthorizationDenied);
        }
        owner
            .pairing
            .restore(saved.key(), saved.client().parts().client.id)
            .await
            .map_err(classify_pairing)?
            .signing_key()
            .await
            .map_err(classify_pairing)?;
        let client = if registration.desired_state() == DesiredServerState::Stopped {
            saved.client().parts().client.id
        } else {
            self.provision_from_owner(registration, &owner, admission, false)
                .await?
        };
        let expected = admission
            .owner_attempt("recovery")
            .map_err(classify_service)?;
        recovery.complete = true;
        let bytes =
            serde_json::to_vec(&recovery).map_err(|_| ManagementFailure::UnsafeNativeState)?;
        admission
            .retain_owner_attempt("recovery", expected.as_deref(), &bytes)
            .map_err(classify_service)?;
        Ok(CliOutput::Enrolled { server, client })
    }

    async fn recovery_journal(
        &self,
        registration: &RegisteredServer,
        admission: &ClientProvisioning,
        primary: &PairingOwner,
        consumer: &str,
    ) -> Result<PairingOwner, ManagementFailure> {
        if consumer == "enrollment" {
            return Ok(PairingOwner {
                pairing: primary.pairing.clone(),
                routes: primary.routes.clone(),
                invalidator: primary.invalidator.clone(),
            });
        }
        if consumer == "cli"
            && let Some(owner) = self.owner.get()
        {
            return Ok(PairingOwner {
                pairing: owner.pairing.clone(),
                routes: owner.routes.clone(),
                invalidator: owner.invalidator.clone(),
            });
        }
        let route = LocalRouteAuthority::new(registration.endpoint().map_err(classify_service)?)
            .map_err(classify_client)?;
        Ok(pairing_owner(
            open_journal(admission, consumer).await?,
            self.credentials.clone(),
            NativeRouteConnector::new(vec![route], None).map_err(classify_client)?,
        ))
    }

    async fn recovery_still_active(
        &self,
        registration: &RegisteredServer,
        owner: &PairingOwner,
        recovery: &AccountRecovery,
    ) -> Result<bool, ManagementFailure> {
        let current = self
            .native
            .supervisor
            .retained_account_recovery()
            .map_err(classify_service)?;
        if current
            .as_deref()
            .map(decode)
            .transpose()?
            .is_none_or(|value| {
                value.preparation.request != recovery.preparation.request || !value.complete
            })
        {
            return Err(ManagementFailure::AuthorizationDenied);
        }
        let inventory = owner.pairing.inventory().await.map_err(classify_pairing)?;
        let Some(saved) = inventory
            .saved
            .iter()
            .find(|saved| saved.credential().scope == recovery.preparation.authentication_scope)
        else {
            return Ok(false);
        };
        let PairingReceiptBody::Enrollment { keys, .. } = &saved.receipt().parts().claims else {
            return Err(ManagementFailure::AuthorizationDenied);
        };
        for (scope, expected_key) in [
            (
                &recovery.preparation.device_scope,
                keys.parts().device_identity_public_key,
            ),
            (
                &recovery.preparation.authentication_scope,
                keys.parts().pairing_authentication_public_key,
            ),
        ] {
            let Some(credential) = self
                .credentials
                .recover_preparation(scope)
                .await
                .map_err(classify_pairing)?
            else {
                return Ok(false);
            };
            if credential.scope != *scope || credential.public_key != expected_key {
                return Err(ManagementFailure::AuthorizationDenied);
            }
        }
        if registration.desired_state() == DesiredServerState::Stopped {
            return Ok(true);
        }
        match open_pairing(
            NativeConsumerKind::Cli,
            owner,
            false,
            Some(registration.server_pin()),
        )
        .await
        {
            Ok(opened) => {
                opened._runtime.shutdown().await.map_err(classify_client)?;
                Ok(true)
            }
            Err(
                ManagementFailure::Revoked
                | ManagementFailure::AuthorizationDenied
                | ManagementFailure::PairingRequired,
            ) => Ok(false),
            Err(error) => Err(error),
        }
    }
}

pub(in crate::native) fn require_complete(
    supervisor: &spaceterm_host::account_service::ServiceSupervisor,
) -> Result<(), ManagementFailure> {
    if let Some(bytes) = supervisor
        .retained_account_recovery()
        .map_err(classify_service)?
        && !decode(&bytes)?.complete
    {
        return Err(ManagementFailure::OutcomePending);
    }
    Ok(())
}
