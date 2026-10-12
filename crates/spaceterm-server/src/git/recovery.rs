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
