//! Restricted self-revocation/status/acknowledgement. This module exposes no
//! organization, terminal, management, bootstrap or enrollment operation.

use super::service::{
    PairingActorRequest, PairingMutationContext, PairingService, PreparedPairingMutation, no_change,
};
use spaceterm_core::{
    operations::{OperationBinding, RequestDigest},
    pairing::*,
};
use spaceterm_protocol::{
    connection::RestrictedConnection,
    pairing_adapter::{operation_from_wire, target_from_wire},
    wire::pairing,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RestrictedRevocationResult {
    Executed(SignedPairingReceipt),
    AlreadyRevoked(SignedPairingReceipt),
    Pending,
    Acknowledged,
}
impl PairingService {
    /// Caller passes the established restricted channel, not a raw Principal.
    /// The actor rechecks the registered purpose and canonical request digest.
    pub async fn restricted_exchange(
        &self,
        connection: &RestrictedConnection,
        request: pairing::RestrictedExtension,
    ) -> Result<RestrictedRevocationResult, PairingFailure> {
        use pairing::restricted_extension::Kind;
        let (operation, digest, target, kind) = match request.kind {
            Some(Kind::Revoke(v)) => (v.operation, v.request_digest, v.target, 1),
            Some(Kind::Status(v)) => (v.operation, v.request_digest, v.target, 2),
            Some(Kind::Acknowledgement(v)) if v.acknowledged_through == 1 => {
                (v.operation, v.request_digest, v.target, 3)
            }
            _ => return Err(PairingFailure::InvalidRequest),
        };
        let operation =
            operation_from_wire(operation.as_ref().ok_or(PairingFailure::InvalidRequest)?)
                .map_err(|_| PairingFailure::InvalidRequest)?;
        let target = target_from_wire(target.as_ref().ok_or(PairingFailure::InvalidRequest)?)
            .map_err(|_| PairingFailure::InvalidRequest)?;
        let digest = RequestDigest::from_bytes(
            digest
                .as_slice()
                .try_into()
                .map_err(|_| PairingFailure::InvalidRequest)?,
        );
        let principal = connection.principal();
        let local = |value: u64| {
            let mut bytes = [0; 16];
            bytes[8..].copy_from_slice(&value.to_be_bytes());
            bytes
        };
        if principal.server != target.parts().server.bytes()
            || principal.device != target.parts().device.bytes()
            || principal.pairing != local(target.parts().pairing.local())
            || principal.client != local(operation.stream().client().local())
            || operation.stream().pairing() != target.parts().pairing
            || operation.sequence().get() != 1
        {
            return Err(PairingFailure::IdentityMismatch);
        }
        let request = self.self_revoke_request(operation, target.clone())?;
        let binding = OperationBinding {
            id: operation,
            digest,
        };
        if request.binding() != binding {
            return Err(PairingFailure::RequestIdentityReused);
        }
        if kind == 3 {
            self.actor().restricted_acknowledge(binding, target).await?;
            return Ok(RestrictedRevocationResult::Acknowledged);
        }
        if let Some(receipt) = self
            .actor()
            .restricted_status(binding, target.clone())
            .await?
        {
            return Ok(match &receipt.parts().claims {
                PairingReceiptBody::Revocation {
                    receipt: claims, ..
                } if claims.operation == operation => RestrictedRevocationResult::Executed(receipt),
                _ => RestrictedRevocationResult::AlreadyRevoked(receipt),
            });
        }
        if kind == 2 {
            return Ok(RestrictedRevocationResult::Pending);
        }
        let service = self.clone();
        let result = self
            .actor()
            .execute(
                PairingActorRequest::Operation(request),
                Box::new(move |context: &mut dyn PairingMutationContext| {
                    let state = context.snapshot();
                    if let Some(receipt) = &state
                        .authorization(target.parts().pairing)?
                        .parts()
                        .revocation_receipt
                    {
                        return Ok(PreparedPairingMutation {
                            transition: no_change(
                                state,
                                PairingOutcome::AlreadyRevoked(receipt.clone()),
                            ),
                            authorization: None,
                        });
                    }
                    let receipt = service.revocation_receipt(state, &target, binding)?;
                    Ok(PreparedPairingMutation {
                        transition: spaceterm_core::pairing::policy::PairingPolicy::default()
                            .prepare_self_revocation(state, &target, binding, receipt)?,
                        authorization: None,
                    })
                }),
            )
            .await?;
        match result.outcome {
            PairingOutcome::Revoked(receipt) => Ok(RestrictedRevocationResult::Executed(receipt)),
            PairingOutcome::AlreadyRevoked(receipt) => {
                Ok(RestrictedRevocationResult::AlreadyRevoked(receipt))
            }
            _ => Err(PairingFailure::InvalidDurableState),
        }
    }
}
