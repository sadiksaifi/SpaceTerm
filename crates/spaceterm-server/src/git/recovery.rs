//! Restart observes external state without constructing an execution permit or repeating a write.
use super::authority::GitCompletionPlan;
use super::host::{GitReadCancellation, HostRecoveryObservation};
use super::writes::GitService;
use spaceterm_core::worktrees::*;
use std::collections::BTreeSet;
use std::sync::Arc;
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitRecoveryReport {
    pub reconciled: usize,
}
impl GitService {
    /// Called with validated store recovery, before ordinary mutation admission.
    pub async fn recover(
        &self,
        intents: Vec<GitWriteIntent>,
    ) -> Result<GitRecoveryReport, GitFailure> {
        let _recovery = self.inner.shutdown.lock().await;
        if intents.len() > self.inner.limits.pending_writes {
            return Err(GitFailure::Capacity);
        }
        {
            let state = self
                .inner
                .state
                .lock()
                .map_err(|_| GitFailure::StorageFailure)?;
            if state.ready || state.closing || state.uncertain || !state.deferred.is_empty() {
                return Err(GitFailure::Busy);
            }
        }
        let mut seen = BTreeSet::new();
        for intent in &intents {
            if intent.binding().id.stream().server() != self.inner.server {
                return Err(GitFailure::WrongServer);
            }
            if !seen.insert(intent.binding().id) {
                return Err(GitFailure::InvalidResponse);
            }
        }
        // Restore every logical gate before the first native observation or completion.
        let mut pending = Vec::new();
        for intent in intents {
            let reservation = self.inner.organization.recover_reservation(&intent).await?;
            if reservation.intent() != &intent {
                return Err(GitFailure::InvalidResponse);
            }
            pending.push((reservation, intent));
        }
        let mut reconciled = 0;
        let mut failure = None;
        for (reservation, intent) in pending {
            let host = Arc::clone(&self.inner.host);
            let observed_intent = intent.clone();
            let observation = match tokio::task::spawn_blocking(move || {
                host.reconcile(&observed_intent, &GitReadCancellation::default())
            })
            .await
            {
                Ok(Ok(observation)) => observation,
                _ => HostRecoveryObservation {
                    observation: RecoveryObservation::Unavailable,
                    external: RetainedExternalResult {
                        disposition: ExternalDisposition::Unavailable,
                        failure: Some(GitFailure::RecoveryNeedsReview),
                        ended_sessions: 0,
                    },
                },
            };
            let completion = GitCompletionPlan {
                intent,
                external: observation.external,
                recovery: Some(observation.observation),
            };
            match self.settle(reservation, completion, None, None).await {
                Ok(_) => reconciled += 1,
                Err(error) => {
                    failure = Some(error);
                }
            }
        }
        if let Some(failure) = failure {
            return Err(failure);
        }
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| GitFailure::StorageFailure)?;
        if !state.deferred.is_empty() {
            return Err(GitFailure::IndeterminateDurability);
        }
        state.ready = true;
        Ok(GitRecoveryReport { reconciled })
    }
}

use crate::adapters::core::{
    CommitCursor, CorePorts, ReductionRequest, observed_native_retirement, observed_retirement,
};
use crate::contracts::{
    ExternalWorkPort, RecoveryEdits, ServerClock, ServerFailure, ServerFailureClass, ServerFuture,
};
use crate::failure::{failure, ledger_failure, store_failure};
use spaceterm_core::{
    operation_ledger_contracts::*, operations::Outcome, organization::ServerDirectory,
    retirement::*, storage::*,
};
use std::sync::{Mutex, OnceLock};

/// Native startup injects this provider before recovery and binds the live Git
/// service before opening listeners. It owns no database or second commit stream.
pub struct GitExternalWork {
    identity: spaceterm_host::organization_store::StoreIdentity,
    home: ServerDirectory,
    clock: Arc<dyn ServerClock>,
    host: Arc<dyn super::host::ServerGitHost>,
    service: OnceLock<GitService>,
    inventory_lease: Mutex<
        Option<(
            spaceterm_core::authority::StoreRevision,
            Arc<super::host::HostInventoryLease>,
        )>,
    >,
}
impl GitExternalWork {
    pub fn new(
        identity: spaceterm_host::organization_store::StoreIdentity,
        home: ServerDirectory,
        clock: Arc<dyn ServerClock>,
        host: Arc<dyn super::host::ServerGitHost>,
    ) -> Result<Self, ServerFailure> {
        if identity.server != home.server() {
            return Err(failure(ServerFailureClass::ScopeMismatch));
        }
        Ok(Self {
            identity,
            home,
            clock,
            host,
            service: OnceLock::new(),
            inventory_lease: Mutex::new(None),
        })
    }
    /// Bind once, after recovery and before listener admission. A mismatched
    /// service cannot drain or publish another server's Git operations.
    pub fn bind(&self, service: GitService) -> Result<(), ServerFailure> {
        if service.inner.server != self.identity.server {
            return Err(failure(ServerFailureClass::ScopeMismatch));
        }
        self.service
            .set(service)
            .map_err(|_| failure(ServerFailureClass::StaleAuthority))
    }
    fn progress_parts(
        &self,
        recovered: &RecoveredState,
        intent: &GitWriteIntent,
        reference: ExternalIntentReference,
        retirement: Option<RetirementDescriptor>,
        writes: Vec<OperationWrite>,
    ) -> Result<TransactionParts, ServerFailure> {
        let mut core = CorePorts::production_with_git_host(
            self.identity,
            self.home.clone(),
            self.clock.clone(),
            self.host.clone(),
        )?;
        core.ledger.recover(recovered)?;
        let cursor = CommitCursor::from_recovered(recovered);
        let mut parts = cursor.parts().map_err(store_failure)?;
        parts.operation_writes.extend(writes);
        let progress = PendingExternalProgress::new(PendingExternalProgressParts {
            binding: intent.binding(),
            effect: ExternalEffectKind::GitWrite,
            state: ExternalProgressState::RecordedIntent {
                reference,
                retirement: retirement.map(Box::new),
            },
        })
        .map_err(ledger_failure)?;
        core.ledger
            .prepare_external_progress(
                intent.binding(),
                progress,
                cursor.context().map_err(ledger_failure)?,
            )?
            .append_to(&mut parts)
            .map_err(ledger_failure)?;
        Ok(parts)
    }
    async fn step(
        &self,
        recovered: &RecoveredState,
        cleanup: &[SessionCleanupObservation],
    ) -> Result<Option<TransactionParts>, ServerFailure> {
        let Some(intent) = recovered.git_intents.first() else {
            return self.restore_step(recovered, cleanup).await;
        };
        let cursor = CommitCursor::from_recovered(recovered);
        // A transferred native batch's cleanup must be durable before Git can
        // delete membership shared by that batch. Preserve its first reason.
        for native in &recovered.native_retirements {
            let observed = observed_native_retirement(native, cleanup)?;
            if &observed != native {
                let mut parts = cursor.parts().map_err(store_failure)?;
                parts
                    .operation_writes
                    .push(OperationWrite::NativeRetirement(
                        NativeRetirementMutation::Progress {
                            expected: Box::new(native.clone()),
                            next: Box::new(observed),
                        },
                    ));
                return Ok(Some(parts));
            }
        }
        let pending = recovered
            .ledger
            .as_ref()
            .ok_or_else(|| failure(ServerFailureClass::RecoveryRequired))?
            .operations()
            .iter()
            .find_map(|entry| match entry {
                RecoveredOperationRecord::Pending(record) if record.binding == intent.binding() => {
                    Some(record.as_ref())
                }
                _ => None,
            })
            .ok_or_else(|| failure(ServerFailureClass::RecoveryRequired))?;
        let Some(ExternalProgressState::RecordedIntent {
            reference,
            retirement,
        }) = pending.external_progress.as_ref().map(|v| v.state())
        else {
            return Err(failure(ServerFailureClass::RecoveryRequired));
        };
        let retirement = retirement
            .as_deref()
            .map(|v| observed_retirement(v, cleanup))
            .transpose()?;
        if retirement.as_ref()
            != match pending.external_progress.as_ref().map(|v| v.state()) {
                Some(ExternalProgressState::RecordedIntent { retirement, .. }) => {
                    retirement.as_deref()
                }
                _ => None,
            }
        {
            return self
                .progress_parts(recovered, intent, *reference, retirement, vec![])
                .map(Some);
        }
        let host = self.host.clone();
        let observed = intent.clone();
        let observation = tokio::task::spawn_blocking(move || {
            host.reconcile(&observed, &GitReadCancellation::default())
        })
        .await
        .map_err(|_| failure(ServerFailureClass::NativeUnavailable))?
        .unwrap_or(HostRecoveryObservation {
            observation: RecoveryObservation::Unavailable,
            external: RetainedExternalResult {
                disposition: ExternalDisposition::Unavailable,
                failure: Some(GitFailure::RecoveryNeedsReview),
                ended_sessions: 0,
            },
        });
        if intent.phase() == GitIntentPhase::Started {
            let reference = ExternalIntentReference::new(ExternalIntentReferenceParts {
                binding: intent.binding(),
                scope: reference.scope(),
                phase: ExternalIntentPhase::ExternalFinished,
                recorded_at: cursor
                    .store
                    .checked_successor()
                    .ok_or_else(|| failure(ServerFailureClass::Exhausted))?,
            })
            .map_err(ledger_failure)?;
            return self
                .progress_parts(
                    recovered,
                    intent,
                    reference,
                    retirement,
                    vec![OperationWrite::GitIntent(GitIntentMutation::Advance {
                        binding: intent.binding(),
                        expected_phase: GitIntentPhase::Started,
                        next: GitIntentPhase::ExternalFinished,
                        result: Some(observation.external),
                    })],
                )
                .map(Some);
        }
        let mut core = CorePorts::production_with_git_host(
            self.identity,
            self.home.clone(),
            self.clock.clone(),
            self.host.clone(),
        )?;
        core.ledger.recover(recovered)?;
        core.reducer
            .recover(recovered, recovered.last_epoch, cleanup)?;
        core.reducer.not_applied();
        let mut external = observation.external;
        if intent.phase() == GitIntentPhase::Prepared {
            // Prepared never authorized an external write. Observed filesystem
            // state is retained as recovery data, not historical write success.
            external.failure = Some(GitFailure::RecoveryNeedsReview);
        }
        let mut reduction = core.reducer.prepare(
            ReductionRequest::SettleStartedGit {
                intent,
                result: &external,
                revoked_clients: &[intent.binding().id.stream().client()],
            },
            cursor,
        )?;
        let revision = reduction.snapshot.state().revision;
        reduction.outcome = Outcome::Git(
            GitMutationOutcome::new(GitMutationOutcomeParts::Recovered {
                server: cursor.server,
                observation: observation.observation,
                revision: (revision != cursor.organization).then_some(revision),
            })
            .map_err(|_| failure(ServerFailureClass::RecoveryRequired))?,
        );
        let mut parts = cursor.parts().map_err(store_failure)?;
        reduction.append_to(&mut parts)?;
        let basis = OperationReductionBasis::new(OperationReductionBasisParts {
            binding: intent.binding(),
            base: cursor.organization,
            resulting: revision,
        })
        .map_err(ledger_failure)?;
        parts
            .operation_writes
            .push(OperationWrite::ReductionBasis(basis));
        core.ledger
            .prepare_completion(
                intent.binding(),
                &reduction.outcome,
                revision,
                TransactionContext::new(cursor.store, cursor.organization, revision, Some(basis))
                    .map_err(ledger_failure)?,
            )?
            .append_to(&mut parts)
            .map_err(ledger_failure)?;
        Ok(Some(parts))
    }
}
impl ExternalWorkPort for GitExternalWork {
    fn cancel_reads(&self) {
        if let Some(service) = self.service.get() {
            let _ = service.quiesce();
        }
    }
    fn finish_started_writes(&self) -> ServerFuture<'_, Result<(), ServerFailure>> {
        Box::pin(async move {
            let service = self
                .service
                .get()
                .ok_or_else(|| failure(ServerFailureClass::NativeUnavailable))?;
            service
                .shutdown()
                .await
                .map(|_| ())
                .map_err(|_| failure(ServerFailureClass::CleanupIncomplete))
        })
    }
    fn reconcile<'a>(
        &'a self,
        _recovered: &'a RecoveredState,
    ) -> ServerFuture<'a, Result<RecoveryEdits, ServerFailure>> {
        // This provider's proposals require exact native cleanup. The owner calls
        // prepare_recovery_step only after validating that evidence.
        Box::pin(async {
            Ok(RecoveryEdits {
                transactions: vec![],
            })
        })
    }
    fn prepare_recovery_step<'a>(
        &'a self,
        recovered: &'a RecoveredState,
        cleanup: &'a [SessionCleanupObservation],
    ) -> ServerFuture<'a, Result<Option<TransactionParts>, ServerFailure>> {
        Box::pin(async move { self.step(recovered, cleanup).await })
    }
}
impl GitExternalWork {
    async fn restore_step(
        &self,
        recovered: &RecoveredState,
        cleanup: &[SessionCleanupObservation],
    ) -> Result<Option<TransactionParts>, ServerFailure> {
        if let Some((expected, lease)) = self
            .inventory_lease
            .lock()
            .map_err(|_| failure(ServerFailureClass::RecoveryRequired))?
            .take()
        {
            if recovered.store_revision != expected {
                return Err(failure(ServerFailureClass::RecoveryRequired));
            }
            lease
                .retain_committed()
                .map_err(|_| failure(ServerFailureClass::RecoveryRequired))?;
        }
        let mut core = CorePorts::production_with_git_host(
            self.identity,
            self.home.clone(),
            self.clock.clone(),
            self.host.clone(),
        )?;
        core.reducer
            .recover(recovered, recovered.last_epoch, cleanup)?;
        core.reducer.not_applied();
        let basis = super::authority::GitAuthoritySnapshot {
            organization: core.reducer.snapshot().as_ref().clone(),
            repositories: recovered.git_repositories.clone(),
            home: self.home.clone(),
            display_directories: vec![],
            runtime: spaceterm_core::worktrees::assessment::RuntimeWorkSet { panes: vec![] },
        };
        let state = basis.organization.state();
        let mut sources = Vec::new();
        for workspace in &state.workspaces {
            let source = workspace
                .pinned_directory
                .value
                .clone()
                .or_else(|| {
                    state
                        .tabs
                        .iter()
                        .find(|v| v.id == workspace.root_tab)
                        .and_then(|tab| state.panes.iter().find(|v| v.id == tab.root_pane))
                        .map(|pane| {
                            pane.confirmed_restoration_directory
                                .value
                                .as_ref()
                                .unwrap_or(&pane.starting_directory)
                                .clone()
                        })
                })
                .unwrap_or_else(|| self.home.clone());
            if !sources.contains(&source) {
                sources.push(source);
            }
        }
        for source in state
            .repositories
            .iter()
            .map(|v| &v.directory)
            .chain(state.worktrees.iter().map(|v| &v.directory))
        {
            if !sources.contains(source) {
                sources.push(source.clone());
            }
        }
        for source in sources {
            let host = self.host.clone();
            let observation_basis = basis.clone();
            let observation = tokio::task::spawn_blocking(move || {
                let cancellation = GitReadCancellation::default();
                let inventory = host.inventory(&source, &observation_basis, &cancellation)?;
                let Some(inventory) = inventory else {
                    return Ok(None);
                };
                let memberships = super::reads::inventory_memberships(
                    host.as_ref(),
                    &observation_basis,
                    &inventory,
                    &cancellation,
                )?;
                Ok::<_, GitFailure>(Some((inventory, memberships)))
            })
            .await
            .map_err(|_| failure(ServerFailureClass::NativeUnavailable))?;
            let Ok(Some((inventory, memberships))) = observation else {
                // A failed or suppressed read is never an authoritative empty list.
                continue;
            };
            if let Some(lease) = &inventory.lease {
                lease
                    .revalidate()
                    .map_err(|_| failure(ServerFailureClass::NativeUnavailable))?;
            }
            let request = super::authority::GitInventoryReconciliation {
                basis: basis.clone(),
                inventory,
                memberships,
            };
            let cursor = CommitCursor::from_recovered(recovered);
            let mut reduction = core.reducer.prepare_git_inventory(&request, cursor)?;
            reduction.auxiliary_writes.retain(|write| !matches!(write, OperationWrite::GitRepository(GitRepositoryMutation::Put(locator)) if recovered.git_repositories.contains(locator)));
            if reduction.organization_writes.is_empty()
                && reduction.auxiliary_writes.is_empty()
                && reduction.high_water == cursor.high_water
            {
                core.reducer.not_applied();
                continue;
            }
            let mut parts = cursor.parts().map_err(store_failure)?;
            reduction.append_to(&mut parts)?;
            if let Some(lease) = request.inventory.lease {
                *self
                    .inventory_lease
                    .lock()
                    .map_err(|_| failure(ServerFailureClass::RecoveryRequired))? =
                    Some((parts.resulting_store_revision, lease));
            }
            return Ok(Some(parts));
        }
        Ok(None)
    }
}
