//! Composition of the domain owners at the store's transaction and recovery boundary.

use super::{auxiliary::invalid, pairing::PairingReceiptReader};
use spaceterm_core::operation_ledger::{canonical::CanonicalCodec, recovery};
use spaceterm_core::operation_ledger_contracts::*;
use spaceterm_core::operations::{OperationBinding, OperationId, Outcome};
use spaceterm_core::organization::ServerDirectory;
use spaceterm_core::pairing::{
    EnrollmentReservationState, PairingOutcome, PairingReceiptBody, SignedPairingReceipt,
};
use spaceterm_core::reducer::{OrganizationLimits, RetainedOrganization};
use spaceterm_core::storage::*;
use spaceterm_core::worktrees::intent::{GitIntentPhase, GitIntentReference, GitRecoveryLedger};
use std::sync::Arc;

pub(super) struct DomainValidator {
    pub home: ServerDirectory,
    pub receipts: Arc<dyn PairingReceiptReader>,
}

impl DurabilityValidator for DomainValidator {
    fn validate_transaction_projection(
        &self,
        parts: &TransactionParts,
    ) -> Result<(), StoreFailure> {
        let proposal = || -> Result<(), StoreFailure> {
            if parts.server != self.home.server() {
                return Err(StoreFailure::IdentityMismatch);
            }
            for write in &parts.operation_writes {
                match write {
                    OperationWrite::Ledger(value) => match value.as_ref() {
                        LedgerWrite::RegisterStream(header) => invalid(header.validate_shape())?,
                        LedgerWrite::AdmitOperation(record)
                        | LedgerWrite::RecordExternalProgress { record, .. } => {
                            invalid(record.validate_shape())?
                        }
                        LedgerWrite::CompleteOperation { record, .. } => {
                            invalid(record.validate_shape(&CanonicalCodec))?
                        }
                        LedgerWrite::AcknowledgePrefix { .. }
                        | LedgerWrite::RetireStream { .. } => {}
                    },
                    OperationWrite::ReductionBasis(value) => {
                        if value.binding().id.stream().server() != parts.server
                            || value.base() != parts.expected_organization_revision
                            || value.resulting() != parts.resulting_organization_revision
                        {
                            return Err(StoreFailure::InvalidTransaction);
                        }
                    }
                    OperationWrite::Pairing(value) => match value.as_ref() {
                        spaceterm_core::pairing::PairingWrite::PutAuthorization {
                            value, ..
                        } => {
                            invalid(value.validate())?;
                            if let Some(receipt) = &value.parts().revocation_receipt {
                                invalid(self.receipts.validate_receipt(receipt))?;
                            }
                        }
                        spaceterm_core::pairing::PairingWrite::PutEnrollmentReservation {
                            value,
                            ..
                        } => {
                            invalid(value.validate())?;
                            if let spaceterm_core::pairing::EnrollmentReservationState::Consumed {
                                receipt,
                                ..
                            } = &value.parts().state
                            {
                                invalid(self.receipts.validate_receipt(receipt))?;
                            }
                        }
                    },
                    OperationWrite::GitRepository(_) | OperationWrite::GitAllocation(_) => {
                        // These enums wrap checked DTOs or a comparison identity.
                        // Their state-dependent scope, allocation and consumption
                        // checks run in auxiliary SQL against the same snapshot.
                    }
                    OperationWrite::GitIntent(value) => invalid(value.validate())?,
                    OperationWrite::NativeRetirement(value) => invalid(value.validate())?,
                    _ => return Err(StoreFailure::RecoveryRequired),
                }
            }
            Ok(())
        };
        proposal().map_err(super::transaction::proposal_failure)
    }

    fn validate_recovery(&self, recovered: &RecoveredState) -> Result<(), StoreFailure> {
        let ledger = recovered
            .ledger
            .as_ref()
            .ok_or(StoreFailure::InvalidDurableState)?;
        invalid(recovery::validate_recovered_state(recovered))?;
        let mut retirements = Vec::new();
        for record in &ledger.operations {
            if let RecoveredOperationRecord::Pending(record) = record
                && let Some(progress) = &record.external_progress
            {
                let descriptor = match progress.state() {
                    ExternalProgressState::AcceptedRetirement(value) => Some(value),
                    ExternalProgressState::RecordedIntent { retirement, .. } => retirement.as_ref(),
                    ExternalProgressState::AwaitingIntent => None,
                };
                if let Some(descriptor) = descriptor {
                    retirements.push(descriptor.as_ref().clone());
                }
            }
        }
        // The reducer's owned recovery input includes organization and retirement
        // projections, covered by recovery inventory residency headroom. Ledger
        // validation above borrows its retained bytes and maps.
        let retained = RetainedOrganization::new(
            recovered.organization.clone(),
            recovered.high_water,
            self.home.clone(),
        )
        .with_pending_retirements(retirements)
        .with_native_retirements(recovered.native_retirements.clone())
        .with_git_allocations(recovered.git_allocations.clone());
        invalid(retained.validate(OrganizationLimits::default()))?;
        invalid(
            recovered
                .pairing
                .as_ref()
                .ok_or(StoreFailure::InvalidDurableState)?
                .validate_receipts(self.receipts.as_ref()),
        )?;
        self.validate_pairing_outcomes(recovered, ledger)?;
        spaceterm_core::worktrees::reconcile::validate_git_recovery(
            recovered.organization.server,
            recovered.high_water,
            &recovered.git_repositories,
            &recovered.git_intents,
            &recovered.git_allocations,
            &BorrowedLedger(ledger),
        )
    }
}

struct BorrowedLedger<'a>(&'a LedgerRecovery);
impl<'a> BorrowedLedger<'a> {
    fn operation(&self, id: OperationId) -> Option<&'a RecoveredOperationRecord> {
        self.0.operations.iter().find(|v| match v {
            RecoveredOperationRecord::Pending(v) => v.binding.id == id,
            RecoveredOperationRecord::Completed(v) => v.binding.id == id,
        })
    }
}
impl GitRecoveryLedger for BorrowedLedger<'_> {
    fn pending_git_progress(
        &self,
        binding: OperationBinding,
    ) -> Result<Option<GitIntentReference>, StoreFailure> {
        let Some(RecoveredOperationRecord::Pending(record)) = self.operation(binding.id) else {
            return Ok(None);
        };
        if record.binding != binding {
            return Err(StoreFailure::InvalidDurableState);
        }
        Ok(match record.external_progress.as_ref().map(|p| p.state()) {
            Some(ExternalProgressState::RecordedIntent { reference, .. })
                if matches!(reference.scope(), ExternalIntentScope::Git(_)) =>
            {
                Some(GitIntentReference::new(
                    binding,
                    match reference.phase() {
                        ExternalIntentPhase::Prepared => GitIntentPhase::Prepared,
                        ExternalIntentPhase::Started => GitIntentPhase::Started,
                        ExternalIntentPhase::ExternalFinished => GitIntentPhase::ExternalFinished,
                    },
                ))
            }
            _ => None,
        })
    }
    fn is_awaiting_git_intent(&self, binding: OperationBinding) -> Result<bool, StoreFailure> {
        Ok(
            matches!(self.operation(binding.id), Some(RecoveredOperationRecord::Pending(record)) if record.binding == binding && record.kind == PendingKind::External { effect: ExternalEffectKind::GitWrite } && matches!(record.external_progress.as_ref().map(|p| p.state()), Some(ExternalProgressState::AwaitingIntent))),
        )
    }
    fn has_completed_git_result(&self, operation: OperationId) -> Result<bool, StoreFailure> {
        match self.operation(operation) {
            Some(RecoveredOperationRecord::Completed(record)) => Ok(matches!(
                invalid(record.outcome.outcome(&CanonicalCodec))?,
                Outcome::Git(_)
            )),
            _ => Ok(false),
        }
    }
}

impl DomainValidator {
    fn validate_pairing_outcomes(
        &self,
        recovered: &RecoveredState,
        ledger: &LedgerRecovery,
    ) -> Result<(), StoreFailure> {
        let pairing = recovered
            .pairing
            .as_ref()
            .ok_or(StoreFailure::InvalidDurableState)?
            .parts();
        let revoked: std::collections::BTreeSet<_> = pairing
            .authorizations
            .iter()
            .filter(|authorization| {
                matches!(
                    authorization.parts().record.state,
                    spaceterm_core::pairing::PairingState::Revoked { .. }
                )
            })
            .map(|authorization| authorization.parts().record.id)
            .collect();
        // Clients may register more ordinary streams than their initial binding
        // records name. Revocation fences every such header, not only that pair.
        if ledger.streams.iter().any(|header| {
            header.purpose == StreamPurpose::Ordinary
                && header.status == StreamStatus::Active
                && revoked.contains(&header.binding.pairing())
        }) {
            return Err(StoreFailure::InvalidDurableState);
        }
        // Both directions matter: a signed result cannot replace authorization,
        // and authorization cannot commit with an unrelated compact result.
        // Acknowledgement alone may remove the original result, not its receipt.
        for authorization in &pairing.authorizations {
            if let Some(receipt) = &authorization.parts().revocation_receipt {
                validate_original_receipt_result(ledger, receipt, None)?;
            }
        }
        for enrollment in &pairing.enrollments {
            if let EnrollmentReservationState::Consumed {
                enrollment,
                receipt,
            } = &enrollment.parts().state
            {
                validate_original_receipt_result(ledger, receipt, Some(*enrollment))?;
            }
        }
        for operation in &ledger.operations {
            let RecoveredOperationRecord::Completed(record) = operation else {
                continue;
            };
            let Outcome::Pairing(outcome) = invalid(record.outcome.outcome(&CanonicalCodec))?
            else {
                continue;
            };
            let proof = match outcome.as_ref() {
                PairingOutcome::Enrolled(proof) => {
                    if proof.parts().claims.revision() != record.completed_at || !pairing.enrollments.iter().any(|v| matches!(&v.parts().state, EnrollmentReservationState::Consumed { enrollment, receipt } if *enrollment == record.binding && receipt.as_ref() == proof)) {
                        return Err(StoreFailure::InvalidDurableState);
                    }
                    proof
                }
                PairingOutcome::Revoked(proof) | PairingOutcome::AlreadyRevoked(proof) => {
                    if !pairing
                        .authorizations
                        .iter()
                        .any(|v| v.parts().revocation_receipt.as_ref() == Some(proof))
                        || proof.parts().claims.revision() > record.completed_at
                    {
                        return Err(StoreFailure::InvalidDurableState);
                    }
                    if matches!(outcome.as_ref(), PairingOutcome::Revoked(_))
                        && (!matches!(&proof.parts().claims, PairingReceiptBody::Revocation { receipt, .. } if receipt.operation == record.binding.id)
                            || proof.parts().claims.revision() != record.completed_at)
                    {
                        return Err(StoreFailure::InvalidDurableState);
                    }
                    proof
                }
                PairingOutcome::ClientBound(proof) => {
                    let PairingReceiptBody::ClientBinding {
                        target,
                        keys,
                        client,
                        operation,
                        revision,
                    } = &proof.parts().claims
                    else {
                        return Err(StoreFailure::InvalidDurableState);
                    };
                    if *operation != record.binding
                        || *revision != record.completed_at
                        || !pairing.authorizations.iter().any(|v| {
                            v.parts().record.id == target.parts().pairing
                                && v.parts().record.device == target.parts().device
                                && &v.parts().keys == keys
                                && v.parts().clients.contains(client)
                        })
                    {
                        return Err(StoreFailure::InvalidDurableState);
                    }
                    proof
                }
                _ => continue,
            };
            invalid(self.receipts.validate_receipt(proof))?;
        }
        Ok(())
    }
}
fn validate_original_receipt_result(
    ledger: &LedgerRecovery,
    proof: &SignedPairingReceipt,
    binding: Option<OperationBinding>,
) -> Result<(), StoreFailure> {
    let id = match &proof.parts().claims {
        PairingReceiptBody::Enrollment { enrollment, .. } => enrollment.id,
        PairingReceiptBody::ClientBinding { operation, .. } => operation.id,
        PairingReceiptBody::Revocation { receipt, .. } => receipt.operation,
    };
    let header = ledger
        .streams
        .iter()
        .find(|v| v.binding.stream() == id.stream())
        .ok_or(StoreFailure::InvalidDurableState)?;
    if id.sequence().get() <= header.acknowledged_through {
        return Ok(());
    }
    let Some(RecoveredOperationRecord::Completed(record)) = BorrowedLedger(ledger).operation(id)
    else {
        return Err(StoreFailure::InvalidDurableState);
    };
    if binding.is_some_and(|v| v != record.binding)
        || record.completed_at != proof.parts().claims.revision()
    {
        return Err(StoreFailure::InvalidDurableState);
    }
    let matches = match (
        &proof.parts().claims,
        invalid(record.outcome.outcome(&CanonicalCodec))?,
    ) {
        (PairingReceiptBody::Enrollment { .. }, Outcome::Pairing(v)) => {
            matches!(v.as_ref(), PairingOutcome::Enrolled(v) if v == proof)
        }
        (PairingReceiptBody::ClientBinding { .. }, Outcome::Pairing(v)) => {
            matches!(v.as_ref(), PairingOutcome::ClientBound(v) if v == proof)
        }
        (PairingReceiptBody::Revocation { .. }, Outcome::Pairing(v)) => {
            matches!(v.as_ref(), PairingOutcome::Revoked(v) if v == proof)
        }
        _ => false,
    };
    if !matches {
        return Err(StoreFailure::InvalidDurableState);
    }
    Ok(())
}
