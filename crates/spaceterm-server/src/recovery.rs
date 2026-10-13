//! Validation and old-instance reconciliation finish before any listener opens.

use crate::{
    adapters::{
        core::{CommitCursor, observed_native_retirement, observed_retirement},
        store::StoreWorker,
    },
    api::ServerConfig,
    contracts::*,
    failure::{failure, ledger_failure, store_failure},
};
use spaceterm_core::{authority::*, operation_ledger_contracts::*, retirement::*, storage::*};
use std::collections::BTreeMap;
use std::sync::Arc;

pub(crate) struct RecoveryResult {
    pub store: StoreWorker,
    pub cursor: CommitCursor,
    pub recovered: RecoveredState,
}

pub(crate) async fn recover(
    config: &mut ServerConfig,
    credentials: &dyn ServerCredentialPort,
    external_work: &dyn ExternalWorkPort,
    host_management: &dyn HostManagementPort,
    store: StoreWorker,
) -> Result<RecoveryResult, ServerFailure> {
    config.limits.validate()?;
    let expected_server = config.host.authority.validate_startup()?;
    config
        .host
        .authority
        .validate_credentials(credentials, expected_server)?;
    let mut recovered = store.recover().await?;
    recovered
        .validate_with(config.core.validator.as_ref())
        .map_err(store_failure)?;
    if expected_server != recovered.organization.server {
        return Err(failure(ServerFailureClass::ScopeMismatch));
    }
    let next_epoch = recovered
        .last_epoch
        .checked_successor()
        .ok_or_else(|| failure(ServerFailureClass::Exhausted))?;
    // The provider verifies all previous-instance ownership even with an empty
    // expected list. A new registry's emptiness is never process-cleanup evidence.
    let expected = expected_cleanup(&recovered)?;
    let sessions: Vec<_> = expected.keys().copied().collect();
    let cleanup = host_management
        .previous_instance_cleanup(expected_server, recovered.last_epoch, &sessions)
        .await?;
    validate_cleanup(&expected, &cleanup)?;

    let edits = external_work.reconcile(&recovered).await?;
    if edits.transactions.len() > 64 {
        return Err(failure(ServerFailureClass::Capacity));
    }
    for parts in edits.transactions {
        let cursor = CommitCursor::from_recovered(&recovered);
        if parts.server != cursor.server
            || parts.expected_store_revision != cursor.store
            || parts.expected_organization_revision != cursor.organization
            || parts.expected_epoch != cursor.epoch
            || parts.resulting_epoch != cursor.epoch
            || parts.expected_high_water != cursor.high_water
        {
            return Err(failure(ServerFailureClass::RecoveryRequired));
        }
        let transaction = Arc::new(
            DurableTransaction::new_with_validation(parts, config.core.validator.as_ref())
                .map_err(store_failure)?,
        );
        let receipt = checked_commit(&store, transaction.clone()).await?;
        // Reopen only through the store authority, not filesystem paths or a
        // second auxiliary truth. Revalidate the complete new recovered bundle.
        recovered = store.recover().await?;
        recovered
            .validate_with(config.core.validator.as_ref())
            .map_err(store_failure)?;
        let resulting = transaction.parts();
        if recovered.organization.server != expected_server
            || recovered.store_revision != receipt.store_revision
            || recovered.organization.revision != receipt.organization_revision
            || recovered.last_epoch != resulting.resulting_epoch
            || recovered.high_water != resulting.resulting_high_water
        {
            return Err(failure(ServerFailureClass::RecoveryRequired));
        }
    }
    config.core.ledger.recover(&recovered)?;
    // Resolve original identities before epoch publication. Each cancellation
    // has its own operation basis; cleanup progress precedes final completion.
    let pending = recovered
        .ledger
        .as_ref()
        .map(|ledger| ledger.operations().to_vec())
        .unwrap_or_default();
    for entry in pending {
        let RecoveredOperationRecord::Pending(record) = entry else {
            continue;
        };
        let Some(progress) = &record.external_progress else {
            continue;
        };
        let cursor = CommitCursor::from_recovered(&recovered);
        let (outcome, revision, basis) = match progress.state() {
            ExternalProgressState::AcceptedRetirement(descriptor) => {
                let observed = observed_retirement(descriptor, &cleanup)?;
                if &observed != descriptor.as_ref() {
                    let progress = PendingExternalProgress::new(PendingExternalProgressParts {
                        binding: record.binding,
                        effect: progress.effect(),
                        state: ExternalProgressState::AcceptedRetirement(Box::new(observed)),
                    })
                    .map_err(ledger_failure)?;
                    let mut parts = cursor.parts().map_err(store_failure)?;
                    config
                        .core
                        .ledger
                        .prepare_external_progress(
                            record.binding,
                            progress,
                            cursor.context().map_err(ledger_failure)?,
                        )?
                        .append_to(&mut parts)
                        .map_err(ledger_failure)?;
                    let transaction = Arc::new(
                        DurableTransaction::new_with_validation(
                            parts,
                            config.core.validator.as_ref(),
                        )
                        .map_err(store_failure)?,
                    );
                    let receipt = checked_commit(&store, transaction).await?;
                    config.core.ledger.committed(receipt)?;
                    recovered = store.recover().await?;
                    recovered
                        .validate_with(config.core.validator.as_ref())
                        .map_err(store_failure)?;
                }
                (
                    descriptor
                        .parts()
                        .eventual_result
                        .outcome(config.core.codec.as_ref())
                        .map_err(ledger_failure)?,
                    descriptor.acceptance_revision(),
                    None,
                )
            }
            ExternalProgressState::AwaitingIntent => {
                let basis = OperationReductionBasis::new(OperationReductionBasisParts {
                    binding: record.binding,
                    base: cursor.organization,
                    resulting: cursor.organization,
                })
                .map_err(ledger_failure)?;
                (
                    spaceterm_core::operations::Outcome::Failed(
                        spaceterm_core::operations::OperationFailure::StaleAuthority,
                    ),
                    cursor.organization,
                    Some(basis),
                )
            }
            ExternalProgressState::RecordedIntent { .. } => continue,
        };
        let cursor = CommitCursor::from_recovered(&recovered);
        let mut parts = cursor.parts().map_err(store_failure)?;
        // A crash after allocation but before Prepared still owns burned IDs.
        // Cancel that original AwaitingIntent and abandon its reservation together.
        if matches!(progress.state(), ExternalProgressState::AwaitingIntent)
            && recovered
                .git_allocations
                .iter()
                .any(|v| v.binding() == record.binding)
        {
            parts.operation_writes.push(OperationWrite::GitAllocation(
                spaceterm_core::worktrees::GitAllocationMutation::Abandon {
                    binding: record.binding,
                },
            ));
        }
        if let Some(basis) = basis {
            parts
                .operation_writes
                .push(OperationWrite::ReductionBasis(basis));
        }
        let context = TransactionContext::new(
            cursor.store,
            cursor.organization,
            cursor.organization,
            basis,
        )
        .map_err(ledger_failure)?;
        config
            .core
            .ledger
            .prepare_completion(record.binding, &outcome, revision, context)?
            .append_to(&mut parts)
            .map_err(ledger_failure)?;
        let transaction = Arc::new(
            DurableTransaction::new_with_validation(parts, config.core.validator.as_ref())
                .map_err(store_failure)?,
        );
        let receipt = checked_commit(&store, transaction).await?;
        config.core.ledger.committed(receipt)?;
        recovered = store.recover().await?;
        recovered
            .validate_with(config.core.validator.as_ref())
            .map_err(store_failure)?;
    }
    for descriptor in recovered.native_retirements.clone() {
        let observed = observed_native_retirement(&descriptor, &cleanup)?;
        if observed == descriptor {
            continue;
        }
        let cursor = CommitCursor::from_recovered(&recovered);
        let mut parts = cursor.parts().map_err(store_failure)?;
        parts
            .operation_writes
            .push(OperationWrite::NativeRetirement(
                NativeRetirementMutation::Progress {
                    expected: Box::new(descriptor),
                    next: Box::new(observed),
                },
            ));
        let transaction = Arc::new(
            DurableTransaction::new_with_validation(parts, config.core.validator.as_ref())
                .map_err(store_failure)?,
        );
        let receipt = checked_commit(&store, transaction).await?;
        config.core.ledger.committed(receipt)?;
        recovered = store.recover().await?;
        recovered
            .validate_with(config.core.validator.as_ref())
            .map_err(store_failure)?;
    }
    // Each external step uses a real durable recovery basis. Never apply an
    // invented receipt to a shadow owner to plan later Git transitions.
    let mut settled = false;
    for _ in 0..256 {
        let Some(parts) = external_work
            .prepare_recovery_step(&recovered, &cleanup)
            .await?
        else {
            settled = true;
            break;
        };
        let cursor = CommitCursor::from_recovered(&recovered);
        if parts.server != cursor.server
            || parts.expected_store_revision != cursor.store
            || parts.expected_organization_revision != cursor.organization
            || parts.expected_epoch != cursor.epoch
            || parts.resulting_epoch != cursor.epoch
            || parts.expected_high_water != cursor.high_water
        {
            return Err(failure(ServerFailureClass::RecoveryRequired));
        }
        let transaction = Arc::new(
            DurableTransaction::new_with_validation(parts, config.core.validator.as_ref())
                .map_err(store_failure)?,
        );
        let receipt = checked_commit(&store, transaction).await?;
        recovered = store.recover().await?;
        recovered
            .validate_with(config.core.validator.as_ref())
            .map_err(store_failure)?;
        if recovered.store_revision != receipt.store_revision
            || recovered.organization.revision != receipt.organization_revision
        {
            return Err(failure(ServerFailureClass::RecoveryRequired));
        }
    }
    if !settled {
        return Err(failure(ServerFailureClass::Capacity));
    }
    config.core.ledger.recover(&recovered)?;
    let reduction = config
        .core
        .reducer
        .recover(&recovered, next_epoch, &cleanup)?;
    let mut cursor = CommitCursor::from_recovered(&recovered);
    let mut parts = cursor.parts().map_err(store_failure)?;
    parts.resulting_epoch = next_epoch;
    reduction.append_to(&mut parts)?;
    let context = TransactionContext::new(
        cursor.store,
        cursor.organization,
        parts.resulting_organization_revision,
        None,
    )
    .map_err(ledger_failure)?;
    if let Some(projection) = config
        .core
        .ledger
        .prepare_recovery(&recovered, &cleanup, context)?
    {
        projection.append_to(&mut parts).map_err(ledger_failure)?;
    }
    for descriptor in &recovered.native_retirements {
        if !parts.operation_writes.iter().any(|write| {
            matches!(write, OperationWrite::NativeRetirement(NativeRetirementMutation::Complete { request, batch })
                if *request == descriptor.request() && *batch == descriptor.batch())
        }) {
            return Err(failure(ServerFailureClass::RecoveryRequired));
        }
    }
    if let Some(ledger) = &recovered.ledger {
        for operation in ledger.operations() {
            if let RecoveredOperationRecord::Pending(record) = operation
                && let Some(progress) = &record.external_progress
                && let ExternalProgressState::AcceptedRetirement(descriptor) = progress.state()
                && !parts.operation_writes.iter().any(|write| {
                    matches!(write, OperationWrite::Ledger(write)
                        if matches!(write.as_ref(), LedgerWrite::CompleteOperation { record: completed, .. }
                            if completed.binding == record.binding
                                && completed.outcome_revision == descriptor.acceptance_revision()))
                })
            {
                return Err(failure(ServerFailureClass::RecoveryRequired));
            }
        }
    }
    let transaction = Arc::new(
        DurableTransaction::new_with_validation(parts, config.core.validator.as_ref())
            .map_err(store_failure)?,
    );
    let receipt = checked_commit(&store, transaction.clone()).await?;
    cursor.install(transaction.parts(), receipt)?;
    config.core.reducer.committed(receipt)?;
    config.core.ledger.committed(receipt)?;
    if config.core.reducer.snapshot().epoch() != next_epoch {
        return Err(failure(ServerFailureClass::RecoveryRequired));
    }
    Ok(RecoveryResult {
        store,
        cursor,
        recovered,
    })
}

async fn checked_commit(
    store: &StoreWorker,
    transaction: Arc<DurableTransaction>,
) -> Result<CommitReceipt, ServerFailure> {
    match store
        .commit(transaction.clone())?
        .await
        .map_err(|_| failure(ServerFailureClass::RecoveryRequired))?
    {
        CommitDisposition::Durable(receipt) if transaction.accepts_receipt(receipt) => Ok(receipt),
        CommitDisposition::NotApplied(value) => Err(store_failure(value)),
        _ => Err(failure(ServerFailureClass::RecoveryRequired)),
    }
}
fn expected_cleanup(
    recovered: &RecoveredState,
) -> Result<BTreeMap<SessionKey, spaceterm_core::organization::RetirementReason>, ServerFailure> {
    let mut expected = BTreeMap::new();
    let mut insert = |sessions: &[RetiredSession]| -> Result<(), ServerFailure> {
        for session in sessions {
            if let Some(previous) = expected.insert(session.session(), session.first_reason())
                && previous != session.first_reason()
            {
                return Err(failure(ServerFailureClass::RecoveryRequired));
            }
        }
        Ok(())
    };
    if let Some(ledger) = &recovered.ledger {
        for operation in ledger.operations() {
            if let RecoveredOperationRecord::Pending(record) = operation
                && let Some(progress) = &record.external_progress
            {
                match progress.state() {
                    ExternalProgressState::AcceptedRetirement(v) => insert(v.sessions())?,
                    ExternalProgressState::RecordedIntent {
                        retirement: Some(v),
                        ..
                    } => insert(v.sessions())?,
                    _ => {}
                }
            }
        }
    }
    for native in &recovered.native_retirements {
        insert(native.sessions())?;
    }
    for intent in &recovered.git_intents {
        if matches!(
            intent.phase(),
            spaceterm_core::worktrees::GitIntentPhase::Started
                | spaceterm_core::worktrees::GitIntentPhase::ExternalFinished
        ) {
            for pane in &intent.parts().affected.parts().panes {
                if let Some(session) = pane.session {
                    expected
                        .entry(session)
                        .or_insert(spaceterm_core::organization::RetirementReason::WorktreeRemoval);
                }
            }
        }
    }
    if expected.len() > 64 {
        return Err(failure(ServerFailureClass::RecoveryRequired));
    }
    Ok(expected)
}
fn validate_cleanup(
    expected: &BTreeMap<SessionKey, spaceterm_core::organization::RetirementReason>,
    cleanup: &[SessionCleanupObservation],
) -> Result<(), ServerFailure> {
    if cleanup.len() != expected.len() {
        return Err(failure(ServerFailureClass::CleanupIncomplete));
    }
    let mut seen = std::collections::BTreeSet::new();
    for value in cleanup {
        if !seen.insert(value.session())
            || expected.get(&value.session()) != Some(&value.first_reason())
            || !matches!(value.result(), CleanupResult::Confirmed(_))
        {
            return Err(failure(ServerFailureClass::CleanupIncomplete));
        }
    }
    Ok(())
}
