//! Identified ordinary pairing grammar in both supported majors. Enrollment and
//! self-revocation are deliberately absent. The fallback owns non-pairing traffic.
//! Transient authorization delivery is not part of the retained compact outcome.

use super::service::{PairingService, server_failure};
use crate::{
    api::{ServerHandle, VerifiedPrincipal},
    contracts::{ServerFailure, ServerFuture},
    protocol_dispatch::OrdinaryDispatchPort,
};
use spaceterm_core::{
    authority::FieldVersion, operation_ledger_contracts::CanonicalOperationRequest, operations::*,
    pairing::*,
};
use spaceterm_protocol::{
    ProtocolError,
    codec::Payload,
    convert::identity,
    pairing_adapter::*,
    wire::{pairing, v1, v2},
};
use std::sync::Arc;

pub struct PairingOrdinaryDispatch {
    service: PairingService,
    fallback: Arc<dyn OrdinaryDispatchPort>,
}
impl PairingOrdinaryDispatch {
    pub fn new(service: PairingService, fallback: Arc<dyn OrdinaryDispatchPort>) -> Self {
        Self { service, fallback }
    }
}
fn decode_unpair_assessment(
    value: &pairing::UnpairAssessment,
) -> Result<UnpairAssessment, ProtocolError> {
    let target = target_from_wire(value.target.as_ref().ok_or(ProtocolError::Malformed)?)?;
    UnpairAssessment::new(UnpairAssessmentParts {
        reference: AssessmentRef {
            operation: operation_from_wire(
                value
                    .assessment_operation
                    .as_ref()
                    .ok_or(ProtocolError::Malformed)?,
            )?,
            digest: RequestDigest::from_bytes(
                value
                    .assessment_digest
                    .as_slice()
                    .try_into()
                    .map_err(|_| ProtocolError::Malformed)?,
            ),
        },
        target: target.clone(),
        authorization_version: FieldVersion::new(value.authorization_version),
        clients: value
            .clients
            .iter()
            .map(|client| {
                identity::decode_client(client).map(|id| ClientRecord {
                    id,
                    pairing: target.parts().pairing,
                })
            })
            .collect::<Result<_, _>>()?,
    })
    .map_err(|_| ProtocolError::Malformed)
}
fn encode_unpair_assessment(value: &UnpairAssessment) -> pairing::UnpairAssessment {
    let p = value.parts();
    pairing::UnpairAssessment {
        assessment_operation: Some(operation_to_wire(p.reference.operation)),
        assessment_digest: p.reference.digest.bytes().to_vec(),
        target: Some(target_to_wire(&p.target)),
        authorization_version: p.authorization_version.get(),
        clients: p
            .clients
            .iter()
            .map(|client| identity::encode_client(client.id))
            .collect(),
    }
}
impl OrdinaryDispatchPort for PairingOrdinaryDispatch {
    fn dispatch(
        &self,
        principal: VerifiedPrincipal,
        payload: Payload,
        handle: ServerHandle,
    ) -> ServerFuture<'_, Result<Option<Payload>, ServerFailure>> {
        Box::pin(async move {
            macro_rules! outcome {
                ($p:ident, $id:expr, $revision:expr, $execution:expr) => {{
                    let id = $id;
                    let execution = $execution;
                    let mut result = $p::PairingOutcome {
                        operation: Some(Box::new(operation_to_wire(id))),
                        ..Default::default()
                    };
                    match &execution.outcome {
                        PairingOutcome::AuthorizationReserved { ticket, .. } => {
                            let state = handle.pairing_snapshot().map_err(server_failure)?;
                            let reservation = state.reservation(ticket).map_err(server_failure)?;
                            result.disposition = 1;
                            result.reservation = Some(Box::new(reservation_to_wire(reservation)));
                            if let Some(authorization) = &execution.authorization {
                                result.authorization_delivery =
                                    Some(Box::new(authorization.with_secret(|secret| {
                                        pairing::AuthorizationDelivery {
                                            reservation: Some(reservation_to_wire(reservation)),
                                            single_use_authorization: secret.to_vec(),
                                        }
                                    })));
                            }
                        }
                        PairingOutcome::Enrolled(receipt) => {
                            result.disposition = 2;
                            result.enrollment_receipt = Some(Box::new(receipt_to_wire(receipt)));
                        }
                        PairingOutcome::ClientBound(receipt) => {
                            result.disposition = 3;
                            result.client_binding_receipt =
                                Some(Box::new(receipt_to_wire(receipt)));
                        }
                        PairingOutcome::UnpairAssessed(assessment) => {
                            result.disposition = 4;
                            result.unpair_assessment =
                                Some(Box::new(encode_unpair_assessment(assessment)));
                        }
                        PairingOutcome::Revoked(receipt) => {
                            result.disposition = 5;
                            result.revocation_proof = Some(Box::new(receipt_to_wire(receipt)));
                        }
                        PairingOutcome::AlreadyRevoked(receipt) => {
                            result.disposition = 6;
                            result.revocation_proof = Some(Box::new(receipt_to_wire(receipt)));
                        }
                        PairingOutcome::Failed(error) => {
                            result.disposition = 7;
                            result.classified_failure = pairing_failure_to_wire(*error) as i32;
                        }
                    }
                    $p::Outcome {
                        operation: Some(operation_to_wire(id)),
                        publication_revision: $revision.get(),
                        result: Some($p::outcome::Result::PairingResult(Box::new(result))),
                    }
                }};
            }
            macro_rules! profile {
                ($message:expr, $p:ident, $major:ident) => {{
                    let message = $message;
                    if let Some($p::message::Kind::OperationLookupRequest(value)) = &message.kind {
                        let binding =
                            spaceterm_protocol::convert::operations::$p::decode_lookup_request(
                                value,
                                principal.pairing(),
                                principal.client(),
                            )
                            .map_err(crate::adapters::transport::protocol_failure)?;
                        if let OperationLookup::Completed {
                            result: Outcome::Pairing(retained),
                            publication_revision,
                        } = handle.lookup_state(principal, binding).await?
                        {
                            let execution = super::service::PairingExecution {
                                outcome: *retained,
                                authorization: None,
                            };
                            let completed =
                                outcome!($p, binding.id, publication_revision, &execution);
                            return Ok(Some(Payload::$major($p::Message {
                                kind: Some($p::message::Kind::OperationLookupResult(
                                    $p::OperationLookupResult {
                                        operation: Some(operation_to_wire(binding.id)),
                                        request_digest: binding.digest.bytes().to_vec(),
                                        disposition: Some(
                                            $p::operation_lookup_result::Disposition::Completed(
                                                Box::new(completed),
                                            ),
                                        ),
                                    },
                                )),
                            })));
                        }
                        return self
                            .fallback
                            .dispatch(principal, Payload::$major(message), handle)
                            .await;
                    }
                    let Some($p::message::Kind::Operation(operation)) = &message.kind else {
                        return self
                            .fallback
                            .dispatch(principal, Payload::$major(message), handle)
                            .await;
                    };
                    let Some($p::operation::Action::IdentifiedPairing(value)) = &operation.action
                    else {
                        return self
                            .fallback
                            .dispatch(principal, Payload::$major(message), handle)
                            .await;
                    };
                    let id = identity::decode_operation(
                        operation
                            .id
                            .as_ref()
                            .ok_or_else(|| server_failure(PairingFailure::InvalidRequest))?,
                    )
                    .map_err(crate::adapters::transport::protocol_failure)?;
                    if operation_from_wire(
                        value
                            .operation
                            .as_ref()
                            .ok_or_else(|| server_failure(PairingFailure::InvalidRequest))?,
                    )
                    .map_err(crate::adapters::transport::protocol_failure)?
                        != id
                        || id.stream().pairing() != principal.pairing()
                        || id.stream().client() != principal.client()
                        || value.request != 0
                    {
                        return Err(server_failure(PairingFailure::IdentityMismatch));
                    }
                    let (execution, command) = match value.action.as_ref() {
                        Some($p::pairing_request::Action::IssueAuthorization(value)) => {
                            let proposal =
                                proposal_from_wire(value.proposal.as_ref().ok_or_else(|| {
                                    server_failure(PairingFailure::InvalidRequest)
                                })?)
                                .map_err(crate::adapters::transport::protocol_failure)?;
                            (
                                self.service
                                    .issue_direct_authorization(principal, id, proposal.clone())
                                    .await
                                    .map_err(server_failure)?,
                                PairingCommand::IssueDirectAuthorization { proposal },
                            )
                        }
                        Some($p::pairing_request::Action::AssessUnpair(value)) => {
                            let target =
                                target_from_wire(value.target.as_ref().ok_or_else(|| {
                                    server_failure(PairingFailure::InvalidRequest)
                                })?)
                                .map_err(crate::adapters::transport::protocol_failure)?;
                            (
                                self.service
                                    .assess_unpair(principal, id, target.clone())
                                    .await
                                    .map_err(server_failure)?,
                                PairingCommand::AssessUnpair { target },
                            )
                        }
                        Some($p::pairing_request::Action::UnpairConfirmed(value)) => {
                            let assessment =
                                decode_unpair_assessment(value.assessment.as_ref().ok_or_else(
                                    || server_failure(PairingFailure::InvalidRequest),
                                )?)
                                .map_err(crate::adapters::transport::protocol_failure)?;
                            (
                                self.service
                                    .unpair(principal, id, assessment.clone())
                                    .await
                                    .map_err(server_failure)?,
                                PairingCommand::UnpairConfirmed { assessment },
                            )
                        }
                        Some($p::pairing_request::Action::BindAdditionalClient(value)) => {
                            let target =
                                target_from_wire(value.target.as_ref().ok_or_else(|| {
                                    server_failure(PairingFailure::InvalidRequest)
                                })?)
                                .map_err(crate::adapters::transport::protocol_failure)?;
                            let expected = FieldVersion::new(value.expected_authorization_version);
                            let claim_digest =
                                RequestDigest::from_bytes(
                                    value.claim_digest.as_slice().try_into().map_err(|_| {
                                        server_failure(PairingFailure::InvalidRequest)
                                    })?,
                                );
                            let device = value
                                .device_identity_proof
                                .as_slice()
                                .try_into()
                                .map_err(|_| server_failure(PairingFailure::InvalidRequest))?;
                            let auth = value
                                .pairing_authentication_proof
                                .as_slice()
                                .try_into()
                                .map_err(|_| server_failure(PairingFailure::InvalidRequest))?;
                            (
                                self.service
                                    .bind_additional_client(
                                        principal,
                                        id,
                                        target.clone(),
                                        expected,
                                        claim_digest,
                                        (device, auth),
                                    )
                                    .await
                                    .map_err(server_failure)?,
                                PairingCommand::BindAdditionalClient {
                                    target,
                                    expected_authorization_version: expected,
                                    claim_digest,
                                },
                            )
                        }
                        _ => return Err(server_failure(PairingFailure::AuthorizationDenied)),
                    };
                    let binding = CanonicalOperationRequest::new(
                        id,
                        Command::Pairing(command),
                        self.service.ledger_codec(),
                    )
                    .map_err(|_| server_failure(PairingFailure::InvalidRequest))?
                    .binding();
                    let (revision, retained) = handle.pairing_outcome(binding).await?;
                    if retained != execution.outcome {
                        return Err(server_failure(PairingFailure::InvalidDurableState));
                    }
                    Ok(Some(Payload::$major($p::Message {
                        kind: Some($p::message::Kind::Outcome(outcome!(
                            $p, id, revision, &execution
                        ))),
                    })))
                }};
            }
            match payload {
                Payload::Major1(message) => profile!(message, v1, Major1),
                Payload::Major2(message) => profile!(message, v2, Major2),
            }
        })
    }
}

fn pairing_failure_to_wire(error: PairingFailure) -> pairing::PairingFailure {
    match error {
        PairingFailure::InvalidRequest => pairing::PairingFailure::PairingInvalidRequest,
        PairingFailure::LimitExceeded => pairing::PairingFailure::PairingLimitExceeded,
        PairingFailure::IdentityMismatch => pairing::PairingFailure::PairingIdentityMismatch,
        PairingFailure::FingerprintMismatch => pairing::PairingFailure::PairingFingerprintMismatch,
        PairingFailure::AuthorizationRequired => {
            pairing::PairingFailure::PairingAuthorizationRequired
        }
        PairingFailure::AuthorizationDenied => pairing::PairingFailure::PairingAuthorizationDenied,
        PairingFailure::AuthorizationUnavailable => {
            pairing::PairingFailure::PairingAuthorizationUnavailable
        }
        PairingFailure::PairingRevoked => pairing::PairingFailure::PairingPairingRevoked,
        PairingFailure::ClientNotBound => pairing::PairingFailure::PairingClientNotBound,
        PairingFailure::StaleAssessment => pairing::PairingFailure::PairingStaleAssessment,
        PairingFailure::AuthorizationExpired => {
            pairing::PairingFailure::PairingAuthorizationExpired
        }
        PairingFailure::AuthorizationConsumed => {
            pairing::PairingFailure::PairingAuthorizationConsumed
        }
        PairingFailure::RequestIdentityReused => {
            pairing::PairingFailure::PairingRequestIdentityReused
        }
        PairingFailure::Offline => pairing::PairingFailure::PairingOffline,
        PairingFailure::RouteChanged => pairing::PairingFailure::PairingRouteChanged,
        PairingFailure::TimedOut => pairing::PairingFailure::PairingTimedOut,
        PairingFailure::Cancelled => pairing::PairingFailure::PairingCancelled,
        PairingFailure::Incompatible => pairing::PairingFailure::PairingIncompatible,
        PairingFailure::UnsupportedTarget => pairing::PairingFailure::PairingUnsupportedTarget,
        PairingFailure::ServiceManagerUnavailable => {
            pairing::PairingFailure::PairingServiceManagerUnavailable
        }
        PairingFailure::ExplicitStartRequired => {
            pairing::PairingFailure::PairingExplicitStartRequired
        }
        PairingFailure::CredentialUnavailable => {
            pairing::PairingFailure::PairingCredentialUnavailable
        }
        PairingFailure::CredentialIntegrity => pairing::PairingFailure::PairingCredentialIntegrity,
        PairingFailure::StorageUnavailable => pairing::PairingFailure::PairingStorageUnavailable,
        PairingFailure::UnsafeStorage => pairing::PairingFailure::PairingUnsafeStorage,
        PairingFailure::UnknownSchema => pairing::PairingFailure::PairingUnknownSchema,
        PairingFailure::InvalidDurableState => pairing::PairingFailure::PairingInvalidDurableState,
        PairingFailure::RecoveryRequired => pairing::PairingFailure::PairingRecoveryRequired,
        PairingFailure::ReceiptInvalid => pairing::PairingFailure::PairingReceiptInvalid,
        PairingFailure::RevocationUnconfirmed => {
            pairing::PairingFailure::PairingRevocationUnconfirmed
        }
    }
}
