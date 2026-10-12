//! Actor/host-port tests. Real reducer/ledger/SQLite composition is a separate integration gate.
use spaceterm_core::authority::*;
use spaceterm_core::identity::*;
use spaceterm_core::operations::*;
use spaceterm_core::organization::*;
use spaceterm_core::storage::*;
use spaceterm_core::worktrees::queries::GitQueryResult;
use spaceterm_core::worktrees::*;
use spaceterm_server::contracts::ConnectionAccessSnapshot;
use spaceterm_server::git::authority::*;
use spaceterm_server::git::host::*;
use spaceterm_server::git::writes::*;
use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::Duration;

fn server() -> ServerId {
    ServerId::new([6; 16])
}
fn directory(value: &[u8]) -> ServerDirectory {
    ServerDirectory::new(server(), value.to_vec()).unwrap()
}
fn principal() -> AuthenticatedGitPrincipal {
    let client = ClientId::new(server(), 1).unwrap();
    AuthenticatedGitPrincipal::from_connection_access(&ConnectionAccessSnapshot {
        server: server(),
        device: DeviceId::new([8; 16]),
        pairing: PairingId::new(server(), 1).unwrap(),
        client,
        binding: ConnectionBinding::new(client, ConnectionGeneration::new(1)),
        epoch: ServerEpoch::new(1),
        reconciled_revision: OrganizationRevision::new(1),
        effects_enabled: true,
    })
    .unwrap()
}
fn operation(sequence: u64) -> OperationId {
    OperationId::new(
        OperationStreamId::new(
            OperationStreamKey::new(server(), 1).unwrap(),
            PairingId::new(server(), 1).unwrap(),
            ClientId::new(server(), 1).unwrap(),
        )
        .unwrap(),
        OperationSequence::new(sequence).unwrap(),
    )
}
fn plan() -> ConfirmedGitPlan {
    let repository = RepositoryLocator::new(RepositoryLocatorParts {
        scope: RepositoryScope {
            server: server(),
            repository: RepositoryId::new(server(), 1).unwrap(),
        },
        main_root: directory(b"/repository"),
        common_directory: directory(b"/repository/.git"),
    })
    .unwrap();
    let draft = WorktreeCreateDraft::new(
        repository.scope(),
        WorktreeBranchChoice::Existing {
            name: BranchName::new("topic".into()).unwrap(),
        },
        WorktreeLocationChoice::Explicit(directory(b"/new")),
    )
    .unwrap();
    ConfirmedGitPlan {
        repository,
        action: ConfirmedGitAction::Create {
            preview: WorktreeCreationPreview::new(draft, directory(b"/new"), None).unwrap(),
            target: CreationTargetObservation::Absent,
        },
        assessment_digest: GitAssessmentDigest::from_bytes([3; 32]),
    }
}
fn intent(
    plan: &ConfirmedGitPlan,
    binding: OperationBinding,
    phase: GitIntentPhase,
) -> (GitWriteIntent, GitAllocationReservation) {
    let ConfirmedGitAction::Create { preview, target } = &plan.action else {
        let ConfirmedGitAction::Remove(scope) = &plan.action else {
            unreachable!()
        };
        let old = IdentityHighWater {
            workspace: 1,
            tab: 1,
            pane: 1,
            creation_ordinal: 3,
            repository: 1,
            worktree: 2,
            pairing: 1,
            client: 1,
            operation_stream: 1,
        };
        let fallbacks = scope
            .retained()
            .parts()
            .fallbacks
            .iter()
            .enumerate()
            .map(|(i, plan)| GitReservedFallback {
                plan: plan.clone(),
                tab: TabId::new(server(), 2 + i as u64).unwrap(),
                pane: PaneId::new(server(), 2 + i as u64).unwrap(),
                tab_creation_ordinal: CreationOrdinal::new(4 + i as u64 * 2),
                pane_creation_ordinal: CreationOrdinal::new(5 + i as u64 * 2),
            })
            .collect::<Vec<_>>();
        let count = fallbacks.len() as u64;
        let allocation = GitAllocationReservation::new(GitAllocationReservationParts {
            binding,
            expected_high_water: old,
            resulting_high_water: IdentityHighWater {
                tab: old.tab + count,
                pane: old.pane + count,
                creation_ordinal: old.creation_ordinal + 2 * count,
                ..old
            },
            repository: None,
            worktree: None,
            fallbacks,
        })
        .unwrap();
        let intent = GitWriteIntent::new(GitWriteIntentParts {
            binding,
            repository: plan.repository.clone(),
            action: RetainedGitAction::Remove {
                worktree: scope.facts().target.id,
                directory: scope.facts().target.directory.clone(),
                head: scope.facts().head.clone(),
                approval: scope.approval(),
            },
            phase,
            assessment_digest: plan.assessment_digest,
            affected: scope.retained().clone(),
            allocation: binding,
            external_result: None,
        })
        .unwrap();
        return (intent, allocation);
    };
    let worktree = WorktreeId::new(server(), 2).unwrap();
    let old = IdentityHighWater {
        repository: 1,
        worktree: 1,
        pairing: 1,
        client: 1,
        operation_stream: 1,
        ..IdentityHighWater::default()
    };
    let allocation = GitAllocationReservation::new(GitAllocationReservationParts {
        binding,
        expected_high_water: old,
        resulting_high_water: IdentityHighWater { worktree: 2, ..old },
        repository: None,
        worktree: Some(worktree),
        fallbacks: vec![],
    })
    .unwrap();
    let intent = GitWriteIntent::new(GitWriteIntentParts {
        binding,
        repository: plan.repository.clone(),
        action: RetainedGitAction::Create {
            worktree,
            directory: preview.parts().directory.clone(),
            branch: preview.parts().draft.parts().branch.clone(),
            target: *target,
            template_version: preview.parts().template_version,
        },
        phase,
        assessment_digest: plan.assessment_digest,
        affected: RetainedRemovalScope::empty(server()),
        allocation: binding,
        external_result: None,
    })
    .unwrap();
    (intent, allocation)
}
fn receipt() -> CommitReceipt {
    CommitReceipt {
        store_revision: StoreRevision::new(12),
        organization_revision: OrganizationRevision::new(77),
    }
}
#[derive(Default)]
struct Actor {
    events: Arc<Mutex<Vec<&'static str>>>,
    pending: Mutex<BTreeMap<OperationId, (GitWriteIntent, GitAllocationReservation)>>,
    outcomes: Mutex<BTreeMap<OperationId, (RequestDigest, GitMutationOutcome)>>,
    prepared: Mutex<VecDeque<CommitDisposition>>,
    started: Mutex<VecDeque<CommitDisposition>>,
    final_commits: Mutex<VecDeque<CommitDisposition>>,
    revoked: AtomicBool,
    quiescing: AtomicBool,
    closed: AtomicBool,
    reject_quota: AtomicBool,
    reserves: AtomicUsize,
    completions: AtomicUsize,
}
impl Actor {
    fn disposition(queue: &Mutex<VecDeque<CommitDisposition>>) -> CommitDisposition {
        queue
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(CommitDisposition::Durable(receipt()))
    }
}
impl GitOrganizationPort for Actor {
    fn authorize(&self, _: AuthenticatedGitPrincipal) -> GitFuture<'_, Result<(), GitFailure>> {
        Box::pin(async move {
            if self.revoked.load(Ordering::Acquire) {
                Err(GitFailure::Unauthorized)
            } else {
                Ok(())
            }
        })
    }
    fn lookup<'a>(
        &'a self,
        _: AuthenticatedGitPrincipal,
        id: OperationId,
        command: &'a GitCommand,
    ) -> GitFuture<'a, Result<GitOperationLookup, GitFailure>> {
        Box::pin(async move {
            // The codec provider is a fake in this suite, not a production legacy-JSON fallback.
            let digest = RequestDigest::for_command(&Command::Worktree(command.clone())).unwrap();
            if let Some((old, outcome)) = self.outcomes.lock().unwrap().get(&id) {
                return if digest == *old {
                    Ok(GitOperationLookup::Completed(outcome.clone()))
                } else {
                    Err(GitFailure::StaleAssessment)
                };
            }
            if let Some((intent, _)) = self.pending.lock().unwrap().get(&id) {
                return if digest == intent.binding().digest {
                    Ok(GitOperationLookup::Pending(intent.binding()))
                } else {
                    Err(GitFailure::StaleAssessment)
                };
            }
            Ok(GitOperationLookup::New(OperationBinding { id, digest }))
        })
    }
    fn reserve_confirmed(
        &self,
        request: ConfirmedGitAdmission,
    ) -> GitFuture<'_, Result<GitReservation, GitFailure>> {
        Box::pin(async move {
            self.events.lock().unwrap().push("admission");
            if self.reject_quota.load(Ordering::Acquire) {
                return Err(GitFailure::Capacity);
            }
            self.reserves.fetch_add(1, Ordering::AcqRel);
            let (intent, allocation) =
                intent(&request.plan, request.binding, GitIntentPhase::Prepared);
            self.pending
                .lock()
                .unwrap()
                .insert(request.binding.id, (intent.clone(), allocation.clone()));
            GitReservation::new(intent, allocation)
        })
    }
    fn persist_prepared<'a>(&'a self, _: &'a GitReservation) -> GitFuture<'a, CommitDisposition> {
        Box::pin(async move {
            self.events.lock().unwrap().push("prepared");
            Self::disposition(&self.prepared)
        })
    }
    fn barrier_and_revalidate<'a>(
        &'a self,
        _: &'a GitReservation,
    ) -> GitFuture<'a, Result<(), GitFailure>> {
        Box::pin(async move {
            self.events.lock().unwrap().push("barrier");
            Ok(())
        })
    }
    fn persist_started<'a>(
        &'a self,
        reservation: &'a GitReservation,
    ) -> GitFuture<'a, CommitDisposition> {
        Box::pin(async move {
            self.events.lock().unwrap().push("started");
            let disposition = Self::disposition(&self.started);
            if matches!(disposition, CommitDisposition::Durable(_)) {
                let mut pending = self.pending.lock().unwrap();
                let record = pending.get_mut(&reservation.binding().id).unwrap();
                let mut parts = record.0.parts().clone();
                parts.phase = GitIntentPhase::Started;
                record.0 = GitWriteIntent::new(parts).unwrap();
            }
            disposition
        })
    }
    fn retire_affected<'a>(
        &'a self,
        reservation: &'a GitReservation,
    ) -> GitFuture<'a, Result<GitRetirementReport, GitFailure>> {
        Box::pin(async move {
            self.events.lock().unwrap().push("retire");
            Ok(GitRetirementReport {
                cleaned: reservation
                    .intent()
                    .parts()
                    .affected
                    .parts()
                    .panes
                    .iter()
                    .filter_map(|p| p.session)
                    .collect(),
            })
        })
    }
    fn complete<'a>(
        &'a self,
        reservation: &'a GitReservation,
        completion: &'a GitCompletionPlan,
    ) -> GitFuture<'a, GitCompletionCommit> {
        Box::pin(async move {
            self.events.lock().unwrap().push("complete");
            self.completions.fetch_add(1, Ordering::AcqRel);
            let disposition = Self::disposition(&self.final_commits);
            if !matches!(disposition, CommitDisposition::Durable(_)) {
                return GitCompletionCommit {
                    disposition,
                    outcome: None,
                };
            }
            // C24: completion is service-owned and does not reject a revoked principal or quiescence.
            let parts = if let Some(observation) = completion.recovery {
                GitMutationOutcomeParts::Recovered {
                    server: server(),
                    observation,
                    revision: Some(receipt().organization_revision),
                }
            } else if let Some(failure) = completion.external.failure {
                GitMutationOutcomeParts::Failed {
                    server: server(),
                    failure,
                    ended_sessions: completion.external.ended_sessions,
                    external: completion.external.disposition,
                }
            } else if matches!(
                completion.intent.parts().action,
                RetainedGitAction::Remove { .. }
            ) {
                GitMutationOutcomeParts::Removed {
                    worktree: completion.intent.parts().action.worktree(),
                    closed_tabs: completion.intent.parts().affected.parts().tabs.len() as u32,
                    ended_sessions: completion.external.ended_sessions,
                    revision: receipt().organization_revision,
                }
            } else {
                GitMutationOutcomeParts::Created {
                    worktree: reservation.intent().parts().action.worktree(),
                    revision: receipt().organization_revision,
                }
            };
            let outcome = GitMutationOutcome::new(parts).unwrap();
            self.outcomes.lock().unwrap().insert(
                reservation.binding().id,
                (reservation.binding().digest, outcome.clone()),
            );
            self.pending
                .lock()
                .unwrap()
                .remove(&reservation.binding().id);
            GitCompletionCommit {
                disposition,
                outcome: Some(outcome),
            }
        })
    }
    fn release(&self, _: GitReservation) -> GitFuture<'_, ()> {
        Box::pin(async move {
            self.events.lock().unwrap().push("release");
        })
    }
    fn recover_reservation<'a>(
        &'a self,
        intent: &'a GitWriteIntent,
    ) -> GitFuture<'a, Result<GitReservation, GitFailure>> {
        Box::pin(async move {
            self.events.lock().unwrap().push("recover-gates");
            let (_, allocation) = self
                .pending
                .lock()
                .unwrap()
                .get(&intent.binding().id)
                .unwrap()
                .clone();
            GitReservation::new(intent.clone(), allocation)
        })
    }
    fn close_mutation_admission(&self) -> GitFuture<'_, ()> {
        Box::pin(async move {
            self.closed.store(true, Ordering::Release);
        })
    }
}
struct Host {
    events: Arc<Mutex<Vec<&'static str>>>,
    executions: AtomicUsize,
    preparations: AtomicUsize,
    reconciliations: AtomicUsize,
    finishes: Arc<AtomicUsize>,
    gate: Mutex<bool>,
    changed: Condvar,
    entered: Mutex<Option<mpsc::Sender<()>>>,
    failure: AtomicBool,
}
impl Host {
    fn new(actor: &Actor) -> Self {
        Self {
            events: Arc::clone(&actor.events),
            executions: AtomicUsize::new(0),
            preparations: AtomicUsize::new(0),
            reconciliations: AtomicUsize::new(0),
            finishes: Arc::new(AtomicUsize::new(0)),
            gate: Mutex::new(false),
            changed: Condvar::new(),
            entered: Mutex::new(None),
            failure: AtomicBool::new(false),
        }
    }
    fn block(&self) -> mpsc::Receiver<()> {
        *self.gate.lock().unwrap() = true;
        let (sender, receiver) = mpsc::channel();
        *self.entered.lock().unwrap() = Some(sender);
        receiver
    }
    fn unblock(&self) {
        *self.gate.lock().unwrap() = false;
        self.changed.notify_all();
    }
}
struct Assessment(Arc<Host>);
struct Prepared {
    host: Arc<Host>,
    observation: HostWriteObservation,
}
impl GitHostAssessment for Assessment {
    fn prepare(
        &self,
        intent: &GitWriteIntent,
        _: &GitReadCancellation,
    ) -> Result<Box<dyn PreparedServerGitWrite>, GitFailure> {
        Ok(Box::new(Prepared {
            host: Arc::clone(&self.0),
            observation: HostWriteObservation {
                binding: intent.binding(),
                assessment_digest: intent.parts().assessment_digest,
            },
        }))
    }
}
impl ServerGitHost for Host {
    fn prepare_write(
        &self,
        request: HostGitWriteRequest,
        cancellation: &GitReadCancellation,
    ) -> Result<Box<dyn PreparedServerGitWrite>, GitFailure> {
        self.preparations.fetch_add(1, Ordering::AcqRel);
        if cancellation.is_cancelled() {
            return Err(GitFailure::CancelledRead);
        }
        request.native.prepare(&request.intent, cancellation)
    }
    fn reconcile(
        &self,
        _: &GitWriteIntent,
        _: &GitReadCancellation,
    ) -> Result<HostRecoveryObservation, GitFailure> {
        self.reconciliations.fetch_add(1, Ordering::AcqRel);
        Ok(HostRecoveryObservation {
            observation: RecoveryObservation::PartialOrAmbiguous,
            external: RetainedExternalResult {
                disposition: ExternalDisposition::PartialOrAmbiguous,
                failure: Some(GitFailure::RecoveryNeedsReview),
                ended_sessions: 0,
            },
        })
    }
    fn shutdown(&self) -> Result<(), GitFailure> {
        self.events.lock().unwrap().push("host-shutdown");
        Ok(())
    }
}
impl PreparedServerGitWrite for Prepared {
    fn observation(&self) -> &HostWriteObservation {
        &self.observation
    }
    fn execute(self: Box<Self>, permit: DurableGitStartPermit) -> HostGitCompletion {
        assert_eq!(permit.binding(), self.observation.binding);
        self.host.events.lock().unwrap().push("execute");
        self.host.executions.fetch_add(1, Ordering::AcqRel);
        if let Some(sender) = self.host.entered.lock().unwrap().as_ref() {
            let _ = sender.send(());
        }
        let mut gate = self.host.gate.lock().unwrap();
        while *gate {
            gate = self.host.changed.wait(gate).unwrap();
        }
        HostGitCompletion {
            result: RetainedExternalResult {
                disposition: ExternalDisposition::DesiredStateObserved,
                failure: self
                    .host
                    .failure
                    .load(Ordering::Acquire)
                    .then_some(GitFailure::WriteFailed),
                ended_sessions: 0,
            },
            settlement: Some(Box::new(Settlement(Arc::clone(&self.host.finishes)))),
        }
    }
}
struct Settlement(Arc<AtomicUsize>);
impl GitHostSettlement for Settlement {
    fn finish(self: Box<Self>, _: bool) -> Result<(), GitFailure> {
        self.0.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }
}
// Native removal validation retains its immutable plan until settlement joins the worker.
struct PlanAssessment {
    host: Arc<Host>,
    plan: Arc<ConfirmedGitPlan>,
}
struct PlanPrepared {
    inner: Box<dyn PreparedServerGitWrite>,
    plan: Arc<ConfirmedGitPlan>,
}
struct PlanSettlement {
    inner: Box<dyn GitHostSettlement>,
    _plan: Arc<ConfirmedGitPlan>,
}
impl GitHostAssessment for PlanAssessment {
    fn prepare(
        &self,
        intent: &GitWriteIntent,
        cancellation: &GitReadCancellation,
    ) -> Result<Box<dyn PreparedServerGitWrite>, GitFailure> {
        Ok(Box::new(PlanPrepared {
            inner: Assessment(self.host.clone()).prepare(intent, cancellation)?,
            plan: self.plan.clone(),
        }))
    }
}
impl PreparedServerGitWrite for PlanPrepared {
    fn observation(&self) -> &HostWriteObservation {
        self.inner.observation()
    }
    fn execute(self: Box<Self>, permit: DurableGitStartPermit) -> HostGitCompletion {
        let mut completion = self.inner.execute(permit);
        completion.settlement = Some(Box::new(PlanSettlement {
            inner: completion.settlement.take().unwrap(),
            _plan: self.plan,
        }));
        completion
    }
}
impl GitHostSettlement for PlanSettlement {
    fn finish(self: Box<Self>, recorded: bool) -> Result<(), GitFailure> {
        self.inner.finish(recorded)
    }
}
async fn fixture() -> (GitService, Arc<Actor>, Arc<Host>, GitCommand) {
    let actor = Arc::new(Actor::default());
    let host = Arc::new(Host::new(&actor));
    let service = GitService::new(
        server(),
        ServerEpoch::new(1),
        host.clone(),
        actor.clone(),
        GitLimits::default(),
    )
    .unwrap();
    service.recover(vec![]).await.unwrap();
    let token = service
        .cache_assessment(principal(), plan(), Arc::new(Assessment(Arc::clone(&host))))
        .await
        .unwrap();
    (
        service,
        actor,
        host,
        GitCommand::CreateConfirmed { assessment: token },
    )
}
async fn outcome(service: &GitService) -> Result<GitMutationOutcome, GitFailure> {
    tokio::time::timeout(Duration::from_secs(10), service.wait(operation(1)))
        .await
        .unwrap()
}

#[tokio::test]
async fn prepared_and_started_intents_commit_before_external_execution() {
    let (service, actor, host, command) = fixture().await;
    service
        .submit(principal(), operation(1), command)
        .await
        .unwrap();
    assert!(
        matches!(outcome(&service).await.unwrap().parts(), GitMutationOutcomeParts::Created { revision, .. } if *revision == OrganizationRevision::new(77))
    );
    let events = actor.events.lock().unwrap().clone();
    assert_eq!(
        events,
        [
            "admission",
            "prepared",
            "barrier",
            "started",
            "execute",
            "complete",
            "release"
        ]
    );
    assert_eq!(host.executions.load(Ordering::Acquire), 1);
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn lost_create_ack_returns_original_outcome_without_second_add() {
    let (service, actor, host, command) = fixture().await;
    service
        .submit(principal(), operation(1), command.clone())
        .await
        .unwrap();
    let original = outcome(&service).await.unwrap();
    assert_eq!(
        service
            .submit(principal(), operation(1), command)
            .await
            .unwrap(),
        GitSubmission::Retained(original)
    );
    assert_eq!(actor.reserves.load(Ordering::Acquire), 1);
    assert_eq!(host.executions.load(Ordering::Acquire), 1);
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn outcome_quota_exhaustion_runs_no_git_and_retires_nothing() {
    let (service, actor, host, command) = fixture().await;
    actor.reject_quota.store(true, Ordering::Release);
    assert_eq!(
        service.submit(principal(), operation(1), command).await,
        Err(GitFailure::Capacity)
    );
    assert_eq!(host.preparations.load(Ordering::Acquire), 0);
    assert!(!actor.events.lock().unwrap().contains(&"retire"));
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn indeterminate_intent_commit_closes_admission() {
    let (service, actor, host, command) = fixture().await;
    actor
        .prepared
        .lock()
        .unwrap()
        .push_back(CommitDisposition::Indeterminate);
    service
        .submit(principal(), operation(1), command.clone())
        .await
        .unwrap();
    assert_eq!(
        outcome(&service).await,
        Err(GitFailure::IndeterminateDurability)
    );
    assert!(actor.closed.load(Ordering::Acquire));
    assert_eq!(host.executions.load(Ordering::Acquire), 0);
    assert_eq!(
        service.submit(principal(), operation(2), command).await,
        Err(GitFailure::IndeterminateDurability)
    );
    assert_eq!(
        service.shutdown().await,
        Err(GitFailure::IndeterminateDurability)
    );
    assert_eq!(service.unresolved_operations().unwrap().len(), 1);
}

#[tokio::test]
async fn indeterminate_final_commit_does_not_repeat_external_write() {
    let (service, actor, host, command) = fixture().await;
    actor
        .final_commits
        .lock()
        .unwrap()
        .push_back(CommitDisposition::Indeterminate);
    service
        .submit(principal(), operation(1), command.clone())
        .await
        .unwrap();
    assert_eq!(
        outcome(&service).await,
        Err(GitFailure::IndeterminateDurability)
    );
    assert!(matches!(
        service
            .submit(principal(), operation(1), command)
            .await
            .unwrap(),
        GitSubmission::Pending(_)
    ));
    assert_eq!(host.executions.load(Ordering::Acquire), 1);
    assert_eq!(
        service.retry_settlements().await,
        Err(GitFailure::IndeterminateDurability)
    );
    assert_eq!(
        service.shutdown().await,
        Err(GitFailure::IndeterminateDurability)
    );
    assert_eq!(service.unresolved_operations().unwrap().len(), 1);
    assert_eq!(host.finishes.load(Ordering::Acquire), 1);
}

#[tokio::test]
async fn unrelated_store_conflict_rebuilds_completion_without_rerunning_git() {
    let (service, actor, host, command) = fixture().await;
    actor
        .final_commits
        .lock()
        .unwrap()
        .push_back(CommitDisposition::NotApplied(
            StoreFailure::RevisionConflict,
        ));
    service
        .submit(principal(), operation(1), command)
        .await
        .unwrap();
    outcome(&service).await.unwrap();
    assert_eq!(actor.completions.load(Ordering::Acquire), 2);
    assert_eq!(host.executions.load(Ordering::Acquire), 1);
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn unavailable_final_storage_retains_completion_and_retries_only_persistence() {
    let (service, actor, host, command) = fixture().await;
    actor
        .final_commits
        .lock()
        .unwrap()
        .push_back(CommitDisposition::NotApplied(StoreFailure::Unavailable));
    service
        .submit(principal(), operation(1), command.clone())
        .await
        .unwrap();
    assert_eq!(outcome(&service).await, Err(GitFailure::StorageUnavailable));
    assert_eq!(host.finishes.load(Ordering::Acquire), 0);
    assert_eq!(service.retry_settlements().await.unwrap(), 1);
    assert!(matches!(
        service
            .submit(principal(), operation(1), command)
            .await
            .unwrap(),
        GitSubmission::Retained(_)
    ));
    assert_eq!(host.executions.load(Ordering::Acquire), 1);
    service.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn deferred_native_settlement_keeps_its_assessment_residency_until_joined() {
    let actor = Arc::new(Actor::default());
    let host = Arc::new(Host::new(&actor));
    let service = GitService::new(
        server(),
        ServerEpoch::new(1),
        host.clone(),
        actor.clone(),
        GitLimits::default(),
    )
    .unwrap();
    service.recover(vec![]).await.unwrap();
    let plan = Arc::new(removal_plan_with_additional_panes(119));
    let proof = Arc::new(PlanAssessment {
        host: host.clone(),
        plan: plan.clone(),
    });
    let token = service
        .cache_assessment(principal(), plan.as_ref().clone(), proof)
        .await
        .unwrap();
    let GitQueryResult::Assessment(page) = service
        .query(
            principal(),
            GitQuery::AssessmentPage {
                assessment: token.clone(),
                cursor: None,
            },
        )
        .await
        .unwrap()
    else {
        panic!("assessment")
    };
    assert!(
        page.next.is_none(),
        "the submitted plan has no cached page retaining its reservation"
    );
    service
        .cache_assessment(
            principal(),
            removal_plan_with_additional_panes(3550),
            Arc::new(Assessment(host.clone())),
        )
        .await
        .unwrap();
    let full = service
        .cache_assessment(
            principal(),
            plan.as_ref().clone(),
            Arc::new(Assessment(host.clone())),
        )
        .await;
    assert_eq!(full, Err(GitFailure::Capacity));
    actor
        .final_commits
        .lock()
        .unwrap()
        .push_back(CommitDisposition::NotApplied(StoreFailure::Unavailable));
    service
        .submit(
            principal(),
            operation(1),
            GitCommand::RemoveConfirmed { assessment: token },
        )
        .await
        .unwrap();
    assert_eq!(outcome(&service).await, Err(GitFailure::StorageUnavailable));
    assert_eq!(
        Arc::strong_count(&plan),
        2,
        "the deferred native worker still holds the plan"
    );
    assert_eq!(host.finishes.load(Ordering::Acquire), 0);
    let after_deferral = service
        .cache_assessment(
            principal(),
            plan.as_ref().clone(),
            Arc::new(Assessment(host.clone())),
        )
        .await;
    if after_deferral != Err(GitFailure::Capacity) {
        assert_eq!(
            service.shutdown().await,
            Err(GitFailure::IndeterminateDurability)
        );
        assert_eq!(
            after_deferral,
            Err(GitFailure::Capacity),
            "deferred native plans remain charged"
        );
    }
    actor
        .final_commits
        .lock()
        .unwrap()
        .push_back(CommitDisposition::NotApplied(StoreFailure::Unavailable));
    assert_eq!(
        service.retry_settlements().await,
        Err(GitFailure::StorageUnavailable)
    );
    let after_retry = service
        .cache_assessment(
            principal(),
            plan.as_ref().clone(),
            Arc::new(Assessment(host.clone())),
        )
        .await;
    if after_retry != Err(GitFailure::Capacity) {
        assert_eq!(
            service.shutdown().await,
            Err(GitFailure::IndeterminateDurability)
        );
        assert_eq!(
            after_retry,
            Err(GitFailure::Capacity),
            "failed settlement retries keep the same reservation"
        );
    }
    tokio::time::advance(Duration::from_secs(121)).await;
    let candidate = removal_plan_with_additional_panes(3640);
    let after_expiration = service
        .cache_assessment(
            principal(),
            candidate.clone(),
            Arc::new(Assessment(host.clone())),
        )
        .await;
    if after_expiration != Err(GitFailure::Capacity) {
        assert_eq!(
            service.shutdown().await,
            Err(GitFailure::IndeterminateDurability)
        );
        assert_eq!(
            after_expiration,
            Err(GitFailure::Capacity),
            "cache expiration must not release an active native plan"
        );
    }
    assert_eq!(service.retry_settlements().await.unwrap(), 1);
    assert_eq!(Arc::strong_count(&plan), 1);
    assert_eq!(host.finishes.load(Ordering::Acquire), 1);
    service
        .cache_assessment(principal(), candidate, Arc::new(Assessment(host.clone())))
        .await
        .unwrap();
    assert_eq!(host.executions.load(Ordering::Acquire), 1);
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn shutdown_waits_for_long_started_write_after_revocation_and_end_all() {
    let (service, actor, host, command) = fixture().await;
    let arrival = host.block();
    service
        .submit(principal(), operation(1), command)
        .await
        .unwrap();
    tokio::task::spawn_blocking(move || arrival.recv_timeout(Duration::from_secs(10)).unwrap())
        .await
        .unwrap();
    actor.revoked.store(true, Ordering::Release);
    actor.quiescing.store(true, Ordering::Release);
    service.quiesce().unwrap();
    let shutdown_service = service.clone();
    let shutdown = tokio::spawn(async move { shutdown_service.shutdown().await });
    tokio::task::yield_now().await;
    assert!(!shutdown.is_finished());
    host.unblock();
    assert!(
        tokio::time::timeout(Duration::from_secs(10), shutdown)
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .unresolved
            .is_empty()
    );
    assert_eq!(actor.outcomes.lock().unwrap().len(), 1);
    assert_eq!(host.executions.load(Ordering::Acquire), 1);
}

#[tokio::test]
async fn dropping_submission_and_waiter_does_not_cancel_started_write() {
    let (service, actor, host, command) = fixture().await;
    let arrival = host.block();
    let _ = service
        .submit(principal(), operation(1), command)
        .await
        .unwrap();
    tokio::task::spawn_blocking(move || arrival.recv_timeout(Duration::from_secs(10)).unwrap())
        .await
        .unwrap();
    let waiter_service = service.clone();
    let waiter = tokio::spawn(async move { waiter_service.wait(operation(1)).await });
    waiter.abort();
    host.unblock();
    service.wait(operation(1)).await.unwrap();
    assert_eq!(actor.outcomes.lock().unwrap().len(), 1);
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn crash_after_started_before_spawn_requires_reconciliation_without_repetition() {
    let actor = Arc::new(Actor::default());
    let host = Arc::new(Host::new(&actor));
    let binding = OperationBinding {
        id: operation(1),
        digest: RequestDigest::from_bytes([7; 32]),
    };
    let (intent, allocation) = intent(&plan(), binding, GitIntentPhase::Started);
    actor
        .pending
        .lock()
        .unwrap()
        .insert(binding.id, (intent.clone(), allocation));
    actor.revoked.store(true, Ordering::Release);
    let service = GitService::new(
        server(),
        ServerEpoch::new(2),
        host.clone(),
        actor.clone(),
        GitLimits::default(),
    )
    .unwrap();
    assert_eq!(service.recover(vec![intent]).await.unwrap().reconciled, 1);
    assert_eq!(host.preparations.load(Ordering::Acquire), 0);
    assert_eq!(host.executions.load(Ordering::Acquire), 0);
    assert_eq!(host.reconciliations.load(Ordering::Acquire), 1);
    assert!(matches!(
        actor
            .outcomes
            .lock()
            .unwrap()
            .get(&binding.id)
            .unwrap()
            .1
            .parts(),
        GitMutationOutcomeParts::Recovered {
            observation: RecoveryObservation::PartialOrAmbiguous,
            ..
        }
    ));
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn real_git_creation_uses_same_service_and_records_observed_inventory_before_success() {
    use spaceterm_host::git::{RepositoryProgramRunner, UnixRepositoryProgramRunner, tools};
    use spaceterm_host::server_directories::native::NativeDirectoryProvider;
    use std::process::{Command as ProcessCommand, Stdio};
    struct Directory(std::path::PathBuf);
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let root = Directory(std::env::temp_dir().join(format!(
        "spaceterm-native-git-service-{}",
        std::process::id()
    )));
    std::fs::create_dir(&root.0).unwrap();
    let repository_path = root.0.join("repository");
    std::fs::create_dir(&repository_path).unwrap();
    let git = |args: &[&str]| {
        assert!(
            ProcessCommand::new("/usr/bin/git")
                .args(args)
                .current_dir(&repository_path)
                .env_clear()
                .env("HOME", &root.0)
                .env("PATH", "/usr/bin:/bin")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success()
        );
    };
    git(&["init", "-b", "main"]);
    git(&[
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@invalid",
        "-c",
        "commit.gpgsign=false",
        "commit",
        "--allow-empty",
        "-m",
        "initial",
    ]);
    let provider = Arc::new(NativeDirectoryProvider::new(server(), root.0.clone()).unwrap());
    let runner: Arc<dyn RepositoryProgramRunner> = Arc::new(UnixRepositoryProgramRunner::new());
    let version = tools::parse_git_version(
        &ProcessCommand::new("/usr/bin/git")
            .arg("--version")
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let host = Arc::new(NativeServerGitHost::new(
        runner,
        "/usr/bin/git".into(),
        root.0.clone(),
        version,
        provider,
    ));
    let actor = Arc::new(Actor::default());
    let repository = RepositoryLocator::new(RepositoryLocatorParts {
        scope: plan().repository.scope(),
        main_root: directory(repository_path.as_os_str().as_encoded_bytes()),
        common_directory: directory(repository_path.join(".git").as_os_str().as_encoded_bytes()),
    })
    .unwrap();
    let draft = WorktreeCreateDraft::new(
        repository.scope(),
        WorktreeBranchChoice::New {
            name: BranchName::new("topic".into()).unwrap(),
            base: BranchReference::Local(BranchName::new("main".into()).unwrap()),
        },
        WorktreeLocationChoice::ServerTemplate {
            expected_version: FieldVersion::new(3),
        },
    )
    .unwrap();
    let settings = ServerSettingsRecord {
        worktree_location: Versioned {
            value: WorktreeLocationTemplate::new("~/.worktrees/{repository}/{branch}".into())
                .unwrap(),
            version: FieldVersion::new(3),
        },
    };
    let preview = host
        .preview_creation(
            draft,
            &repository,
            &settings,
            &GitReadCancellation::default(),
        )
        .unwrap();
    assert_eq!(
        preview.parts().directory,
        directory(
            root.0
                .join(".worktrees/repository/topic")
                .as_os_str()
                .as_encoded_bytes()
        )
    );
    assert_eq!(
        preview.validate_template_version(FieldVersion::new(4)),
        Err(GitFailure::StaleAssessment)
    );
    let plan = ConfirmedGitPlan {
        repository,
        action: ConfirmedGitAction::Create {
            preview: preview.clone(),
            target: CreationTargetObservation::Absent,
        },
        assessment_digest: GitAssessmentDigest::from_bytes([4; 32]),
    };
    let native = host
        .assess(plan.clone(), &GitReadCancellation::default())
        .unwrap();
    let service = GitService::new(
        server(),
        ServerEpoch::new(1),
        host,
        actor,
        GitLimits::default(),
    )
    .unwrap();
    service.recover(vec![]).await.unwrap();
    let token = service
        .cache_assessment(principal(), plan, native)
        .await
        .unwrap();
    let command = GitCommand::CreateConfirmed { assessment: token };
    service
        .submit(principal(), operation(1), command.clone())
        .await
        .unwrap();
    let created = outcome(&service).await.unwrap();
    assert!(
        matches!(created.parts(), GitMutationOutcomeParts::Created { .. }),
        "{created:?}"
    );
    assert!(root.0.join(".worktrees/repository/topic/.git").is_file());
    assert!(matches!(
        service
            .submit(principal(), operation(1), command)
            .await
            .unwrap(),
        GitSubmission::Retained(_)
    ));
    service.shutdown().await.unwrap();
}

#[test]
fn existing_and_remote_branch_operands_cannot_inject_options() {
    for choice in [
        WorktreeBranchChoice::Existing {
            name: BranchName::new("--force".into()).unwrap(),
        },
        WorktreeBranchChoice::Remote {
            remote: BranchName::new("main^{tree}".into()).unwrap(),
            name: BranchName::new("topic".into()).unwrap(),
        },
        WorktreeBranchChoice::New {
            name: BranchName::new("topic".into()).unwrap(),
            base: BranchReference::Local(BranchName::new("main~1".into()).unwrap()),
        },
    ] {
        assert!(matches!(
            spaceterm_server::git::host::branch(&choice),
            Err(GitFailure::InvalidBranch)
        ));
    }
}

fn removal_plan() -> ConfirmedGitPlan {
    removal_plan_with_additional_panes(0)
}
fn removal_plan_with_additional_panes(additional: u64) -> ConfirmedGitPlan {
    use spaceterm_core::server_directories::DirectoryContainment;
    use spaceterm_core::worktrees::assessment::*;
    let workspace = WorkspaceId::new(server(), 1).unwrap();
    let tab = TabId::new(server(), 1).unwrap();
    let pane = PaneId::new(server(), 1).unwrap();
    let repository = plan().repository;
    let target = WorktreeRecord {
        id: WorktreeId::new(server(), 2).unwrap(),
        repository: repository.scope().repository,
        directory: directory(b"/remove"),
        main: false,
        missing: false,
    };
    let main = WorktreeRecord {
        id: WorktreeId::new(server(), 1).unwrap(),
        repository: repository.scope().repository,
        directory: directory(b"/repository"),
        main: true,
        missing: false,
    };
    let generation = SessionGeneration::new(1);
    let epoch = ServerEpoch::new(1);
    let state = OrganizationState {
        server: server(),
        revision: OrganizationRevision::new(1),
        settings: ServerSettingsRecord {
            worktree_location: Versioned {
                value: WorktreeLocationTemplate::new("~/{branch}".into()).unwrap(),
                version: FieldVersion::new(1),
            },
        },
        workspaces: vec![WorkspaceRecord {
            id: workspace,
            root_tab: tab,
            creation_ordinal: CreationOrdinal::new(1),
            name: Versioned {
                value: WorkspaceName {
                    mode: NameMode::Automatic,
                    display: NameText::new("work".into()).unwrap(),
                },
                version: FieldVersion::new(1),
            },
            pinned_directory: Versioned {
                value: None,
                version: FieldVersion::new(1),
            },
        }],
        tabs: vec![TabRecord {
            id: tab,
            workspace,
            worktree: Some(target.id),
            root_pane: pane,
            creation_ordinal: CreationOrdinal::new(2),
        }],
        panes: vec![PaneRecord {
            id: pane,
            tab,
            creation: PaneCreation::Initial,
            creation_ordinal: CreationOrdinal::new(3),
            starting_directory: target.directory.clone(),
            confirmed_restoration_directory: Versioned {
                value: None,
                version: FieldVersion::new(1),
            },
        }],
        repositories: vec![RepositoryRecord {
            id: repository.scope().repository,
            directory: repository.parts().main_root.clone(),
        }],
        worktrees: vec![main.clone(), target.clone()],
        retained: Some({
            use spaceterm_core::organization_retained::*;
            OrganizationRetainedFacts::new(OrganizationRetainedParts {
                server: server(),
                workspace_names: vec![WorkspaceNameBasis::new(
                    workspace,
                    NameText::new("work".into()).unwrap(),
                    NameText::new("work".into()).unwrap(),
                )],
                creation_defaults: vec![TabCreationDefault::new(tab, vec![pane]).unwrap()],
                pane_generations: vec![PaneGenerationHighWater::new(
                    pane,
                    generation,
                    ControlGeneration::new(0),
                )],
                tab_membership_versions: vec![TabMembershipVersion::new(tab, FieldVersion::new(1))],
                repository_inventory_versions: vec![RepositoryInventoryVersion::new(
                    repository.scope().repository,
                    FieldVersion::new(1),
                )],
                worktree_inventory: vec![
                    WorktreeInventoryFacts::new(main.id, true, FieldVersion::new(1)),
                    WorktreeInventoryFacts::new(target.id, true, FieldVersion::new(1)),
                ],
                remote_access: Versioned {
                    value: spaceterm_core::remote_access::RemoteAccessPolicy::disabled(server()),
                    version: FieldVersion::new(1),
                },
            })
            .unwrap()
        }),
    };
    let mut state = state;
    let mut pane_runtime = vec![PaneRuntimeRecord {
        pane,
        session_generation: generation,
        phase: SessionPhase::Running { generation },
        control_generation: ControlGeneration::new(0),
        control: ControlRecord::Unclaimed,
    }];
    if additional != 0 {
        use spaceterm_core::organization_retained::*;
        let mut retained = state.retained.take().unwrap().parts().clone();
        let mut panes = vec![pane];
        for index in 2..=additional + 1 {
            let id = PaneId::new(server(), index).unwrap();
            panes.push(id);
            state.panes.push(PaneRecord {
                id,
                tab,
                creation_ordinal: CreationOrdinal::new(index + 2),
                creation: PaneCreation::SplitOf {
                    source: pane,
                    axis: SplitAxis::Horizontal,
                    edge: SplitEdge::After,
                },
                starting_directory: directory(format!("/{}", "x".repeat(4090)).as_bytes()),
                confirmed_restoration_directory: Versioned {
                    value: None,
                    version: FieldVersion::new(1),
                },
            });
            retained.pane_generations.push(PaneGenerationHighWater::new(
                id,
                SessionGeneration::new(0),
                ControlGeneration::new(0),
            ));
            pane_runtime.push(PaneRuntimeRecord {
                pane: id,
                session_generation: SessionGeneration::new(0),
                phase: SessionPhase::NotStarted,
                control_generation: ControlGeneration::new(0),
                control: ControlRecord::Unclaimed,
            });
        }
        retained.creation_defaults = vec![TabCreationDefault::new(tab, panes).unwrap()];
        state.retained = Some(OrganizationRetainedFacts::new(retained).unwrap());
    }
    let snapshot = OrganizationSnapshot::new(epoch, state, pane_runtime).unwrap();
    let runtime = RuntimeWorkSet {
        panes: snapshot
            .runtime()
            .iter()
            .map(|record| RuntimeWorkStamp {
                pane: record.pane,
                session: (record.pane == pane).then_some(SessionKey {
                    epoch,
                    pane: record.pane,
                    generation,
                }),
                context_generation: ContextGeneration::new(1),
                work_generation: CloseWorkGeneration::new(1),
                startup_reserved: false,
                confirmed_directory: None,
            })
            .collect(),
    };
    let facts = WorktreeRemovalFacts {
        target: target.clone(),
        listed: true,
        locked: false,
        head: None,
        changes: false,
        submodules: false,
    };
    let affected = AffectedWorktreeWork {
        pinned: vec![],
        panes: snapshot
            .state()
            .panes
            .iter()
            .map(|record| PaneDirectoryContainment {
                pane: record.id,
                directory: record.restoration_directory().clone(),
                containment: if record.id == pane {
                    DirectoryContainment::Inside
                } else {
                    DirectoryContainment::Outside
                },
            })
            .collect(),
        fallbacks: vec![FallbackWorktree {
            record: main,
            bare: false,
            available: true,
            containment: DirectoryContainment::Outside,
        }],
    };
    let scope = assess_removal(&snapshot, &runtime, &facts, &affected).unwrap();
    ConfirmedGitPlan {
        repository,
        action: ConfirmedGitAction::Remove(Box::new(scope)),
        assessment_digest: GitAssessmentDigest::from_bytes([9; 32]),
    }
}

#[tokio::test]
async fn started_intent_commits_before_exact_session_retirement_and_removal() {
    let (service, actor, host, _) = fixture().await;
    let token = service
        .cache_assessment(
            principal(),
            removal_plan(),
            Arc::new(Assessment(Arc::clone(&host))),
        )
        .await
        .unwrap();
    let command = GitCommand::RemoveConfirmed { assessment: token };
    service
        .submit(principal(), operation(1), command.clone())
        .await
        .unwrap();
    let original = outcome(&service).await.unwrap();
    assert!(matches!(
        original.parts(),
        GitMutationOutcomeParts::Removed {
            ended_sessions: 1,
            closed_tabs: 1,
            ..
        }
    ));
    assert_eq!(
        *actor.events.lock().unwrap(),
        [
            "admission",
            "prepared",
            "barrier",
            "started",
            "retire",
            "execute",
            "complete",
            "release"
        ]
    );
    assert_eq!(
        service
            .submit(principal(), operation(1), command)
            .await
            .unwrap(),
        GitSubmission::Retained(original)
    );
    assert_eq!(host.executions.load(Ordering::Acquire), 1);
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn git_failure_after_retirement_reports_ended_sessions_without_successful_removal() {
    let (service, _, host, _) = fixture().await;
    host.failure.store(true, Ordering::Release);
    let token = service
        .cache_assessment(
            principal(),
            removal_plan(),
            Arc::new(Assessment(Arc::clone(&host))),
        )
        .await
        .unwrap();
    service
        .submit(
            principal(),
            operation(1),
            GitCommand::RemoveConfirmed { assessment: token },
        )
        .await
        .unwrap();
    assert!(matches!(
        outcome(&service).await.unwrap().parts(),
        GitMutationOutcomeParts::Failed {
            ended_sessions: 1,
            failure: GitFailure::WriteFailed,
            ..
        }
    ));
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn shutdown_cancels_not_started_queue_entries_but_drains_all_started_writes() {
    let (service, actor, host, first) = fixture().await;
    let arrival = host.block();
    service
        .submit(principal(), operation(1), first)
        .await
        .unwrap();
    for sequence in 2..=5 {
        let token = service
            .cache_assessment(principal(), plan(), Arc::new(Assessment(Arc::clone(&host))))
            .await
            .unwrap();
        service
            .submit(
                principal(),
                operation(sequence),
                GitCommand::CreateConfirmed { assessment: token },
            )
            .await
            .unwrap();
    }
    tokio::task::spawn_blocking(move || {
        for _ in 0..4 {
            arrival.recv_timeout(Duration::from_secs(10)).unwrap();
        }
    })
    .await
    .unwrap();
    service.quiesce().unwrap();
    let shutting_down = service.clone();
    let shutdown = tokio::spawn(async move { shutting_down.shutdown().await });
    tokio::task::yield_now().await;
    assert!(!shutdown.is_finished());
    host.unblock();
    tokio::time::timeout(Duration::from_secs(10), shutdown)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(host.executions.load(Ordering::Acquire), 4);
    assert!(matches!(
        actor
            .outcomes
            .lock()
            .unwrap()
            .get(&operation(5))
            .unwrap()
            .1
            .parts(),
        GitMutationOutcomeParts::Failed {
            failure: GitFailure::CancelledRead,
            ended_sessions: 0,
            ..
        }
    ));
}

#[test]
fn bare_repository_without_live_linked_worktree_returns_explainable_refusal() {
    use spaceterm_host::git::{RepositoryProgramRunner, UnixRepositoryProgramRunner, tools};
    use spaceterm_host::server_directories::native::NativeDirectoryProvider;
    use std::process::{Command as ProcessCommand, Stdio};
    struct Directory(std::path::PathBuf);
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let root = Directory(std::env::temp_dir().join(format!(
        "spaceterm-native-bare-assessment-{}",
        std::process::id()
    )));
    std::fs::create_dir(&root.0).unwrap();
    let repository_path = root.0.join("bare.git");
    std::fs::create_dir(&repository_path).unwrap();
    assert!(
        ProcessCommand::new("/usr/bin/git")
            .args(["init", "--bare", "-b", "main"])
            .current_dir(&repository_path)
            .env_clear()
            .env("HOME", &root.0)
            .env("PATH", "/usr/bin:/bin")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
    );
    let provider = Arc::new(NativeDirectoryProvider::new(server(), root.0.clone()).unwrap());
    let runner: Arc<dyn RepositoryProgramRunner> = Arc::new(UnixRepositoryProgramRunner::new());
    let version = tools::parse_git_version(
        &ProcessCommand::new("/usr/bin/git")
            .arg("--version")
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let host = NativeServerGitHost::new(
        runner,
        "/usr/bin/git".into(),
        root.0.clone(),
        version,
        provider,
    );
    let mut plan = plan();
    let directory = directory(repository_path.as_os_str().as_encoded_bytes());
    plan.repository = RepositoryLocator::new(RepositoryLocatorParts {
        scope: plan.repository.scope(),
        main_root: directory.clone(),
        common_directory: directory,
    })
    .unwrap();
    assert!(matches!(
        host.assess(plan, &GitReadCancellation::default()),
        Err(GitFailure::BareRepositoryWithoutWorktree)
    ));
    assert!(!root.0.join("new").exists());
    host.shutdown().unwrap();
}

struct NativeRemovalFixture {
    root: std::path::PathBuf,
    provider: Arc<spaceterm_host::server_directories::native::NativeDirectoryProvider>,
    host: Arc<NativeServerGitHost>,
    repository: RepositoryLocator,
    snapshot: OrganizationSnapshot,
}
impl Drop for NativeRemovalFixture {
    fn drop(&mut self) {
        let _ = self.host.shutdown();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
impl NativeRemovalFixture {
    fn new(name: &str, runner: Arc<dyn spaceterm_host::git::RepositoryProgramRunner>) -> Self {
        use spaceterm_core::organization_retained::*;
        use spaceterm_host::git::tools;
        use spaceterm_host::server_directories::native::NativeDirectoryProvider;
        let root = std::env::temp_dir().join(format!("spaceterm-{name}-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let main_path = root.join("repository");
        std::fs::create_dir(&main_path).unwrap();
        run_git(&main_path, &["init", "-b", "main"]);
        run_git(
            &main_path,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--allow-empty",
                "-m",
                "initial",
            ],
        );
        run_git(
            &main_path,
            &[
                "worktree",
                "add",
                "-b",
                "topic",
                root.join("linked").to_str().unwrap(),
            ],
        );
        let provider = Arc::new(NativeDirectoryProvider::new(server(), root.clone()).unwrap());
        let version = tools::parse_git_version(
            &std::process::Command::new("/usr/bin/git")
                .arg("--version")
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        let host = Arc::new(NativeServerGitHost::new(
            runner,
            "/usr/bin/git".into(),
            root.clone(),
            version,
            Arc::clone(&provider),
        ));
        let repository = RepositoryLocator::new(RepositoryLocatorParts {
            scope: plan().repository.scope(),
            main_root: directory(main_path.as_os_str().as_encoded_bytes()),
            common_directory: directory(main_path.join(".git").as_os_str().as_encoded_bytes()),
        })
        .unwrap();
        let source = removal_plan();
        let ConfirmedGitAction::Remove(scope) = source.action else {
            unreachable!()
        };
        let tab = scope.retained().parts().tabs[0].clone();
        let pane = scope.retained().parts().panes[0].hierarchy.pane;
        let main = WorktreeRecord {
            id: WorktreeId::new(server(), 1).unwrap(),
            repository: repository.scope().repository,
            directory: repository.parts().main_root.clone(),
            main: true,
            missing: false,
        };
        let target = WorktreeRecord {
            id: WorktreeId::new(server(), 2).unwrap(),
            directory: directory(root.join("linked").as_os_str().as_encoded_bytes()),
            main: false,
            ..main.clone()
        };
        let state = OrganizationState {
            server: server(),
            revision: OrganizationRevision::new(1),
            settings: ServerSettingsRecord {
                worktree_location: Versioned {
                    value: WorktreeLocationTemplate::new(
                        "~/.worktrees/{repository}/{branch}".into(),
                    )
                    .unwrap(),
                    version: FieldVersion::new(1),
                },
            },
            workspaces: vec![WorkspaceRecord {
                id: tab.workspace,
                root_tab: tab.id,
                creation_ordinal: CreationOrdinal::new(1),
                name: Versioned {
                    value: WorkspaceName {
                        mode: NameMode::Automatic,
                        display: NameText::new("work".into()).unwrap(),
                    },
                    version: FieldVersion::new(1),
                },
                pinned_directory: Versioned {
                    value: None,
                    version: FieldVersion::new(1),
                },
            }],
            tabs: vec![tab.clone()],
            panes: vec![PaneRecord {
                id: pane,
                tab: tab.id,
                creation: PaneCreation::Initial,
                creation_ordinal: CreationOrdinal::new(3),
                starting_directory: main.directory.clone(),
                confirmed_restoration_directory: Versioned {
                    value: None,
                    version: FieldVersion::new(1),
                },
            }],
            repositories: vec![RepositoryRecord {
                id: main.repository,
                directory: main.directory.clone(),
            }],
            worktrees: vec![main.clone(), target.clone()],
            retained:
                Some(
                    OrganizationRetainedFacts::new(OrganizationRetainedParts {
                        server: server(),
                        workspace_names: vec![WorkspaceNameBasis::new(
                            tab.workspace,
                            NameText::new("work".into()).unwrap(),
                            NameText::new("work".into()).unwrap(),
                        )],
                        creation_defaults: vec![
                            TabCreationDefault::new(tab.id, vec![pane]).unwrap(),
                        ],
                        pane_generations: vec![PaneGenerationHighWater::new(
                            pane,
                            SessionGeneration::new(1),
                            ControlGeneration::new(0),
                        )],
                        tab_membership_versions: vec![TabMembershipVersion::new(
                            tab.id,
                            FieldVersion::new(1),
                        )],
                        repository_inventory_versions: vec![RepositoryInventoryVersion::new(
                            main.repository,
                            FieldVersion::new(1),
                        )],
                        worktree_inventory: vec![
                            WorktreeInventoryFacts::new(main.id, true, FieldVersion::new(1)),
                            WorktreeInventoryFacts::new(target.id, true, FieldVersion::new(1)),
                        ],
                        remote_access: Versioned {
                            value: spaceterm_core::remote_access::RemoteAccessPolicy::disabled(
                                server(),
                            ),
                            version: FieldVersion::new(1),
                        },
                    })
                    .unwrap(),
                ),
        };
        let snapshot = OrganizationSnapshot::new(
            ServerEpoch::new(1),
            state,
            vec![PaneRuntimeRecord {
                pane,
                session_generation: SessionGeneration::new(1),
                phase: SessionPhase::Running {
                    generation: SessionGeneration::new(1),
                },
                control_generation: ControlGeneration::new(0),
                control: ControlRecord::Unclaimed,
            }],
        )
        .unwrap();
        Self {
            root,
            provider,
            host,
            repository,
            snapshot,
        }
    }
    fn plan(&self) -> Result<ConfirmedGitPlan, GitFailure> {
        use spaceterm_core::server_directories::ServerDirectoryQueries;
        use spaceterm_core::worktrees::assessment::*;
        let facts = self.host.removal_facts(
            &self.repository,
            &self.snapshot.state().worktrees[1],
            &GitReadCancellation::default(),
        )?;
        let runtime = RuntimeWorkSet {
            panes: self
                .snapshot
                .runtime()
                .iter()
                .map(|r| RuntimeWorkStamp {
                    pane: r.pane,
                    session: Some(SessionKey {
                        epoch: self.snapshot.epoch(),
                        pane: r.pane,
                        generation: r.session_generation,
                    }),
                    context_generation: ContextGeneration::new(1),
                    work_generation: CloseWorkGeneration::new(1),
                    startup_reserved: false,
                    confirmed_directory: None,
                })
                .collect(),
        };
        let affected = AffectedWorktreeWork {
            pinned: vec![],
            panes: self
                .snapshot
                .state()
                .panes
                .iter()
                .map(|p| PaneDirectoryContainment {
                    pane: p.id,
                    directory: p.restoration_directory().clone(),
                    containment: self
                        .provider
                        .containment(&facts.target.directory, p.restoration_directory())
                        .unwrap(),
                })
                .collect(),
            fallbacks: vec![FallbackWorktree {
                record: self.snapshot.state().worktrees[0].clone(),
                bare: false,
                available: true,
                containment: self
                    .provider
                    .containment(
                        &facts.target.directory,
                        &self.snapshot.state().worktrees[0].directory,
                    )
                    .unwrap(),
            }],
        };
        let scope = assess_removal(&self.snapshot, &runtime, &facts, &affected)?;
        Ok(ConfirmedGitPlan {
            repository: self.repository.clone(),
            action: ConfirmedGitAction::Remove(Box::new(scope)),
            assessment_digest: GitAssessmentDigest::from_bytes([11; 32]),
        })
    }
}
fn run_git(path: &std::path::Path, arguments: &[&str]) {
    use std::process::{Command, Stdio};
    assert!(
        Command::new("/usr/bin/git")
            .args(arguments)
            .current_dir(path)
            .env_clear()
            .env("HOME", path)
            .env("PATH", "/usr/bin:/bin")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
    );
}

#[tokio::test]
async fn shutdown_cancels_pipe_blocked_native_preparation_before_retirement_or_git_mutation() {
    use spaceterm_host::git::*;
    use std::io::Write;
    struct BlockStatus {
        enabled: AtomicBool,
        root: Mutex<Option<std::path::PathBuf>>,
        bounded: AtomicBool,
        finished: AtomicBool,
    }
    impl RepositoryProgramRunner for BlockStatus {
        fn run(
            &self,
            request: &ProgramRequest,
            output: &mut dyn FnMut(&[u8]),
            cancellation: &CancellationToken,
        ) -> Result<ProgramExit, ProgramError> {
            let runner = UnixRepositoryProgramRunner::new();
            if self.enabled.load(Ordering::Acquire)
                && request.arguments.iter().any(|a| a == "status")
            {
                self.bounded
                    .store(request.deadline.is_some(), Ordering::Release);
                let root = self.root.lock().unwrap().clone().unwrap();
                // Run a real native process with a blocked pipe instead of a user's clean filter.
                let blocked = ProgramRequest {
                    executable: "/bin/sh".into(),
                    arguments: vec![
                        "-c".into(),
                        "printf '%s' $$ > \"$1/pid\"; IFS= read -r line < \"$1/pipe\"".into(),
                        "validation".into(),
                        root.into_os_string(),
                    ],
                    directory: request.directory.clone(),
                    environment: request.environment.clone(),
                    stdout_limit: request.stdout_limit,
                    deadline: request.deadline,
                    keep_process_group_on_exit: false,
                };
                let result = runner.run(&blocked, output, cancellation);
                self.finished.store(true, Ordering::Release);
                result
            } else {
                runner.run(request, output, cancellation)
            }
        }
    }
    let runner = Arc::new(BlockStatus {
        enabled: AtomicBool::new(false),
        root: Mutex::new(None),
        bounded: AtomicBool::new(false),
        finished: AtomicBool::new(false),
    });
    let fixture = NativeRemovalFixture::new("cancel-native-validation", runner.clone());
    let plan = fixture.plan().unwrap();
    let native = fixture
        .host
        .assess(plan.clone(), &GitReadCancellation::default())
        .unwrap();
    assert!(
        std::process::Command::new("/usr/bin/mkfifo")
            .arg(fixture.root.join("pipe"))
            .status()
            .unwrap()
            .success()
    );
    let mut pipe = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(fixture.root.join("pipe"))
        .unwrap();
    *runner.root.lock().unwrap() = Some(fixture.root.clone());
    let actor = Arc::new(Actor::default());
    let service = GitService::new(
        server(),
        ServerEpoch::new(1),
        fixture.host.clone(),
        actor.clone(),
        GitLimits::default(),
    )
    .unwrap();
    service.recover(vec![]).await.unwrap();
    let token = service
        .cache_assessment(principal(), plan, native)
        .await
        .unwrap();
    runner.enabled.store(true, Ordering::Release);
    service
        .submit(
            principal(),
            operation(1),
            GitCommand::RemoveConfirmed { assessment: token },
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !fixture.root.join("pid").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let shutting_down = service.clone();
    let mut shutdown = tokio::spawn(async move { shutting_down.shutdown().await });
    let result = tokio::time::timeout(Duration::from_secs(2), &mut shutdown).await;
    let completed_without_release = result.is_ok();
    if !completed_without_release {
        runner.enabled.store(false, Ordering::Release);
        pipe.write_all(b"release\n").unwrap();
        shutdown.await.unwrap().unwrap();
    } else {
        result.unwrap().unwrap().unwrap();
    }
    assert!(
        completed_without_release,
        "shutdown waited for an unstarted native validation instead of cancelling it"
    );
    assert!(runner.bounded.load(Ordering::Acquire));
    assert!(runner.finished.load(Ordering::Acquire));
    let pid = std::fs::read_to_string(fixture.root.join("pid")).unwrap();
    assert!(
        !std::process::Command::new("/bin/kill")
            .args(["-0", &pid])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(
        *actor.events.lock().unwrap(),
        ["admission", "complete", "release"]
    );
    let outcomes = actor.outcomes.lock().unwrap();
    let result = &outcomes.get(&operation(1)).unwrap().1;
    assert!(matches!(
        result.parts(),
        GitMutationOutcomeParts::Failed {
            failure: GitFailure::CancelledRead,
            ended_sessions: 0,
            ..
        }
    ));
    assert!(fixture.root.join("linked/.git").is_file());
    run_git(
        &fixture.root.join("repository"),
        &["show-ref", "--verify", "refs/heads/topic"],
    );
}

#[tokio::test]
async fn real_git_missing_worktree_with_unrelated_pane_directory_can_be_assessed_and_removed() {
    use spaceterm_host::git::UnixRepositoryProgramRunner;
    let mut fixture = NativeRemovalFixture::new(
        "remove-missing",
        Arc::new(UnixRepositoryProgramRunner::new()),
    );
    std::fs::remove_dir_all(fixture.root.join("linked")).unwrap();
    let mut state = fixture.snapshot.state().clone();
    state.worktrees[1].missing = true;
    std::os::unix::fs::symlink(&fixture.root, fixture.root.join("parent-alias")).unwrap();
    state.worktrees[1].directory = directory(
        fixture
            .root
            .join("parent-alias/linked")
            .as_os_str()
            .as_encoded_bytes(),
    );
    fixture.snapshot = OrganizationSnapshot::new(
        fixture.snapshot.epoch(),
        state,
        fixture.snapshot.runtime().to_vec(),
    )
    .unwrap();
    let plan = fixture.plan().unwrap();
    let ConfirmedGitAction::Remove(scope) = &plan.action else {
        unreachable!()
    };
    assert_eq!(scope.approval(), ApprovedRemoval::Missing);
    assert_eq!(scope.retained().parts().fallbacks.len(), 1);
    let native = fixture
        .host
        .assess(plan.clone(), &GitReadCancellation::default())
        .unwrap();
    let actor = Arc::new(Actor::default());
    let service = GitService::new(
        server(),
        fixture.snapshot.epoch(),
        fixture.host.clone(),
        actor.clone(),
        GitLimits::default(),
    )
    .unwrap();
    service.recover(vec![]).await.unwrap();
    let token = service
        .cache_assessment(principal(), plan, native)
        .await
        .unwrap();
    service
        .submit(
            principal(),
            operation(1),
            GitCommand::RemoveConfirmed { assessment: token },
        )
        .await
        .unwrap();
    let result = outcome(&service).await.unwrap();
    assert!(
        matches!(
            result.parts(),
            GitMutationOutcomeParts::Removed {
                closed_tabs: 1,
                ended_sessions: 1,
                ..
            }
        ),
        "{result:?}"
    );
    assert!(
        !fixture
            .root
            .join("repository/.git/worktrees/linked")
            .exists()
    );
    run_git(
        &fixture.root.join("repository"),
        &["show-ref", "--verify", "refs/heads/topic"],
    );
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn object_appearing_at_missing_worktree_refuses_preparation_before_session_retirement() {
    use spaceterm_host::git::UnixRepositoryProgramRunner;
    for kind in 0..3 {
        let mut fixture = NativeRemovalFixture::new(
            &format!("missing-replacement-{kind}"),
            Arc::new(UnixRepositoryProgramRunner::new()),
        );
        let target = fixture.root.join("linked");
        std::fs::remove_dir_all(&target).unwrap();
        let mut state = fixture.snapshot.state().clone();
        state.worktrees[1].missing = true;
        fixture.snapshot = OrganizationSnapshot::new(
            fixture.snapshot.epoch(),
            state,
            fixture.snapshot.runtime().to_vec(),
        )
        .unwrap();
        let plan = fixture.plan().unwrap();
        let native = fixture
            .host
            .assess(plan.clone(), &GitReadCancellation::default())
            .unwrap();
        let actor = Arc::new(Actor::default());
        let service = GitService::new(
            server(),
            fixture.snapshot.epoch(),
            fixture.host.clone(),
            actor.clone(),
            GitLimits::default(),
        )
        .unwrap();
        service.recover(vec![]).await.unwrap();
        let token = service
            .cache_assessment(principal(), plan, native)
            .await
            .unwrap();
        match kind {
            0 => std::fs::create_dir(&target).unwrap(),
            1 => std::fs::write(&target, b"replacement").unwrap(),
            _ => std::os::unix::fs::symlink(fixture.root.join("gone"), &target).unwrap(),
        }
        service
            .submit(
                principal(),
                operation(1),
                GitCommand::RemoveConfirmed { assessment: token },
            )
            .await
            .unwrap();
        let result = outcome(&service).await.unwrap();
        assert!(
            matches!(
                result.parts(),
                GitMutationOutcomeParts::Failed {
                    failure: GitFailure::IdentityReplaced,
                    ended_sessions: 0,
                    external: ExternalDisposition::NoEffectEstablished,
                    ..
                }
            ),
            "{result:?}"
        );
        assert_eq!(
            *actor.events.lock().unwrap(),
            ["admission", "complete", "release"]
        );
        assert!(std::fs::symlink_metadata(&target).is_ok());
        assert!(
            fixture
                .root
                .join("repository/.git/worktrees/linked")
                .is_dir()
        );
        service.shutdown().await.unwrap();
    }
}
