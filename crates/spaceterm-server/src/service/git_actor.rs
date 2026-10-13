//! Git requests share the actor's reducer, ledger and ordered store worker.
use super::*;
use crate::git::authority::*;
use spaceterm_core::worktrees::{assessment::*, *};
use std::collections::VecDeque;

type GitReply<T> = oneshot::Sender<Result<T, ServerFailure>>;
pub(crate) enum GitRequest {
    Snapshot(GitReply<GitAuthoritySnapshot>),
    Inventory(
        Option<AuthenticatedGitPrincipal>,
        GitInventoryReconciliation,
        GitReply<GitAuthoritySnapshot>,
    ),
    Outside(
        AuthenticatedGitPrincipal,
        GitOutsideSourceReconciliation,
        GitReply<GitAuthoritySnapshot>,
    ),
    Authorize(AuthenticatedGitPrincipal, GitReply<()>),
    Lookup(
        AuthenticatedGitPrincipal,
        OperationId,
        GitCommand,
        GitReply<GitOperationLookup>,
    ),
    Admit(ConfirmedGitAdmission, GitReply<OperationBinding>),
    Allocate(
        OperationBinding,
        GitAllocationNeeds,
        GitReply<ReservedGitAllocation>,
    ),
    Reserve(
        ConfirmedGitAdmission,
        GitAllocationReservation,
        GitReply<GitReservation>,
    ),
    Prepared(GitReservation, GitReply<CommitReceipt>),
    Started(GitWriteIntent, GitReply<CommitReceipt>),
    Barrier(GitWriteIntent, GitReply<()>),
    Retire(GitWriteIntent, GitReply<CommitReceipt>),
    Complete(GitCompletionPlan, GitReply<GitCompletionCommit>),
    Release(OperationBinding, GitReply<()>),
    Recover(GitWriteIntent, GitReply<GitReservation>),
    Cancel(OperationBinding, GitFailure, GitReply<()>),
    Close(GitReply<()>),
}
pub(crate) enum GitCommit {
    Admitted(OperationBinding, GitReply<OperationBinding>),
    ExternalFinished(Box<GitCompletionPlan>, GitReply<GitCompletionCommit>),
    Completed(GitMutationOutcome, GitReply<GitCompletionCommit>),
    Cancelled(GitReply<()>),
    Inventory(
        Option<Arc<crate::git::host::HostInventoryLease>>,
        GitReply<GitAuthoritySnapshot>,
    ),
}
#[derive(Default)]
pub(crate) struct GitActorState {
    leases: BTreeMap<OperationId, GitLease>,
    pub(super) waiting: VecDeque<GitRequest>,
}
#[derive(Clone, Copy, Eq, PartialEq)]
enum GitBarrier {
    Open,
    Paused,
    Checked,
}
struct GitLease {
    plan: Option<ConfirmedGitPlan>,
    intent: GitWriteIntent,
    barrier: GitBarrier,
}
impl Actor {
    pub(super) fn git_request(&mut self, request: GitRequest) {
        if self.pending.is_some() || self.busy {
            if self.git.waiting.len() < 64 {
                self.git.waiting.push_back(request);
            } else {
                reject(request, failure(ServerFailureClass::Capacity));
            }
            return;
        }
        match request {
            GitRequest::Inventory(principal, request, reply) => {
                self.git_inventory(principal, request, reply)
            }
            GitRequest::Outside(principal, request, reply) => {
                self.git_outside(principal, request, reply)
            }
            GitRequest::Snapshot(reply) => {
                let _ = reply.send(self.git_snapshot());
            }
            GitRequest::Authorize(principal, reply) => {
                let _ = reply.send(self.git_principal(principal).map(|_| ()));
            }
            GitRequest::Lookup(principal, operation, command, reply) => {
                let result = self.git_lookup(principal, operation, &command);
                let _ = reply.send(result);
            }
            GitRequest::Admit(request, reply) => self.git_admit(request, reply),
            GitRequest::Allocate(binding, needs, reply) => {
                self.reserve_git_allocation(binding, needs, reply)
            }
            GitRequest::Reserve(request, allocation, reply) => {
                let result = self.git_reserve(request, allocation);
                let _ = reply.send(result);
            }
            GitRequest::Prepared(reservation, reply) => self.git_prepared(reservation, reply),
            GitRequest::Started(intent, reply) => self.git_started(intent, reply),
            GitRequest::Barrier(intent, reply) => self.git_barrier(intent, reply),
            GitRequest::Retire(intent, reply) => self.git_retire(intent, reply),
            GitRequest::Complete(completion, reply) => self.git_complete(completion, reply),
            GitRequest::Release(binding, reply) => {
                let result = self.git_release(binding);
                let _ = reply.send(result);
            }
            GitRequest::Recover(intent, reply) => {
                let result = self.git_recover(intent);
                let _ = reply.send(result);
            }
            GitRequest::Cancel(binding, failure, reply) => self.git_cancel(binding, failure, reply),
            GitRequest::Close(reply) => {
                self.enter_recovery();
                let _ = reply.send(Ok(()));
            }
        }
    }
    fn git_principal(
        &self,
        principal: AuthenticatedGitPrincipal,
    ) -> Result<VerifiedPrincipal, ServerFailure> {
        let connection = self
            .connections
            .get(&principal.client())
            .ok_or_else(|| failure(ServerFailureClass::Authentication))?;
        if principal.server() != self.cursor.server
            || principal.epoch() != self.cursor.epoch
            || principal.pairing() != connection.principal.pairing()
        {
            return Err(failure(ServerFailureClass::Authentication));
        }
        self.check_principal(connection.principal, true)?;
        Ok(connection.principal)
    }
    pub(super) fn git_snapshot(&self) -> Result<GitAuthoritySnapshot, ServerFailure> {
        let organization = self.config.core.reducer.snapshot();
        let observations = self
            .registry
            .as_ref()
            .ok_or_else(|| failure(ServerFailureClass::RecoveryRequired))?
            .observations();
        let runtime = RuntimeWorkSet {
            panes: organization
                .runtime()
                .iter()
                .map(|record| {
                    let session = matches!(record.phase, SessionPhase::Running { .. }).then_some(
                        SessionKey {
                            pane: record.pane,
                            epoch: self.cursor.epoch,
                            generation: record.session_generation,
                        },
                    );
                    let observed = observations
                        .iter()
                        .find(|(key, _)| Some(*key) == session)
                        .map(|(_, facts)| facts);
                    let context = match record.control {
                        ControlRecord::Held { client, .. } => {
                            self.connections.get(&client).map(|v| v.context)
                        }
                        ControlRecord::Unclaimed => None,
                    }
                    .unwrap_or(ContextGeneration::new(1));
                    RuntimeWorkStamp {
                        pane: record.pane,
                        session,
                        context_generation: context,
                        work_generation: observed
                            .and_then(|v| v.close_work)
                            .map(|v| v.generation())
                            .unwrap_or(CloseWorkGeneration::new(0)),
                        startup_reserved: matches!(record.phase, SessionPhase::Starting { .. })
                            || self
                                .intents
                                .as_ref()
                                .is_some_and(|v| v.contains_pane(record.pane)),
                        confirmed_directory: observed
                            .and_then(|v| v.confirmed_directory.clone())
                            .filter(|v| {
                                organization
                                    .state()
                                    .panes
                                    .iter()
                                    .find(|p| p.id == record.pane)
                                    .is_some_and(|p| {
                                        p.confirmed_restoration_directory.value.as_ref()
                                            == Some(v.directory())
                                    })
                            }),
                    }
                })
                .collect(),
        };
        Ok(GitAuthoritySnapshot {
            organization: organization.as_ref().clone(),
            repositories: self.git_repositories.clone(),
            home: self
                .config
                .core
                .reducer
                .git_home()
                .ok_or_else(|| failure(ServerFailureClass::NativeUnavailable))?,
            display_directories: observations
                .iter()
                .filter_map(|(_, v)| v.display_directory.clone())
                .collect(),
            runtime,
        })
    }
    fn git_lookup(
        &self,
        principal: AuthenticatedGitPrincipal,
        operation: OperationId,
        command: &GitCommand,
    ) -> Result<GitOperationLookup, ServerFailure> {
        self.git_principal(principal)?;
        if operation.stream().pairing() != principal.pairing()
            || operation.stream().client() != principal.client()
        {
            return Err(failure(ServerFailureClass::ScopeMismatch));
        }
        let binding = CanonicalOperationRequest::new(
            operation,
            Command::Worktree(command.clone()),
            self.config.core.codec.as_ref(),
        )
        .map_err(ledger_failure)?
        .binding();
        match self.config.core.ledger.lookup(binding)? {
            OperationLookup::NeverAdmitted => Ok(GitOperationLookup::New(binding)),
            OperationLookup::Pending => Ok(GitOperationLookup::Pending(binding)),
            OperationLookup::Completed {
                result: Outcome::Git(outcome),
                ..
            } => Ok(GitOperationLookup::Completed(outcome)),
            _ => Err(failure(ServerFailureClass::StaleAuthority)),
        }
    }
    fn git_admit(&mut self, request: ConfirmedGitAdmission, reply: GitReply<OperationBinding>) {
        let prepared = (|| {
            self.available()?;
            self.git_principal(request.principal)?;
            if !self.ordinary_open() || self.terminal_barrier || self.git.leases.len() >= 32 {
                return Err(failure(ServerFailureClass::Capacity));
            }
            if self.git_lookup(request.principal, request.binding.id, &request.command)?
                != GitOperationLookup::New(request.binding)
            {
                return Err(failure(ServerFailureClass::StaleAuthority));
            }
            self.git_revalidate(&request.plan)?;
            let canonical = CanonicalOperationRequest::new(
                request.binding.id,
                Command::Worktree(request.command),
                self.config.core.codec.as_ref(),
            )
            .map_err(ledger_failure)?;
            canonical
                .verify_binding(request.binding)
                .map_err(ledger_failure)?;
            let mut parts = self.cursor.parts().map_err(store_failure)?;
            self.config
                .core
                .ledger
                .prepare_admission(
                    &canonical,
                    PendingKind::External {
                        effect: ExternalEffectKind::GitWrite,
                    },
                    self.cursor.context().map_err(ledger_failure)?,
                )?
                .append_to(&mut parts)
                .map_err(ledger_failure)?;
            Ok(parts)
        })();
        match prepared {
            Ok(parts) => self.commit(
                parts,
                CommitAction::Git(GitCommit::Admitted(request.binding, reply)),
            ),
            Err(error) => {
                let _ = reply.send(Err(error));
            }
        }
    }
    fn git_reserve(
        &mut self,
        request: ConfirmedGitAdmission,
        allocation: GitAllocationReservation,
    ) -> Result<GitReservation, ServerFailure> {
        self.available()?;
        self.git_principal(request.principal)?;
        if !self.ordinary_open()
            || self.terminal_barrier
            || self.config.core.ledger.lookup(request.binding)? != OperationLookup::Pending
            || request.binding != allocation.binding()
            || self.git.leases.contains_key(&request.binding.id)
        {
            return Err(failure(ServerFailureClass::StaleAuthority));
        }
        self.git_revalidate(&request.plan)?;
        let (action, affected) = match &request.plan.action {
            ConfirmedGitAction::Create { preview, target } => (
                RetainedGitAction::Create {
                    worktree: allocation
                        .parts()
                        .worktree
                        .ok_or_else(|| failure(ServerFailureClass::RecoveryRequired))?,
                    directory: preview.parts().directory.clone(),
                    branch: preview.parts().draft.parts().branch.clone(),
                    target: *target,
                    template_version: preview.parts().template_version,
                },
                RetainedRemovalScope::new(RetainedRemovalScopeParts {
                    server: self.cursor.server,
                    tabs: vec![],
                    panes: vec![],
                    membership_versions: vec![],
                    fallbacks: vec![],
                })
                .map_err(git_failure)?,
            ),
            ConfirmedGitAction::Remove(scope) => (
                RetainedGitAction::Remove {
                    worktree: scope.facts().target.id,
                    directory: scope.facts().target.directory.clone(),
                    head: scope.facts().head.clone(),
                    approval: scope.approval(),
                },
                scope.retained().clone(),
            ),
        };
        let intent = GitWriteIntent::new(GitWriteIntentParts {
            binding: request.binding,
            repository: request.plan.repository.clone(),
            action,
            phase: GitIntentPhase::Prepared,
            assessment_digest: request.plan.assessment_digest,
            affected,
            allocation: request.binding,
            external_result: None,
        })
        .map_err(git_failure)?;
        let reservation =
            GitReservation::new(intent.clone(), allocation.clone()).map_err(git_failure)?;
        self.git.leases.insert(
            request.binding.id,
            GitLease {
                plan: Some(request.plan),
                intent,
                barrier: GitBarrier::Open,
            },
        );
        Ok(reservation)
    }
    fn git_revalidate(&self, plan: &ConfirmedGitPlan) -> Result<(), ServerFailure> {
        plan.validate_scope().map_err(git_failure)?;
        let current = self.git_snapshot()?;
        if plan.repository.scope().server != self.cursor.server
            || !current.organization.state().repositories.iter().any(|v| {
                v.id == plan.repository.scope().repository
                    && v.directory == plan.repository.parts().main_root
            })
        {
            return Err(failure(ServerFailureClass::StaleAuthority));
        }
        if self.git.leases.values().any(|lease| {
            lease.intent.parts().repository.scope() == plan.repository.scope()
                && lease.plan.as_ref() != Some(plan)
        }) {
            return Err(failure(ServerFailureClass::Capacity));
        }
        match &plan.action {
            ConfirmedGitAction::Create { preview, .. } => {
                if preview.parts().template_version.is_some_and(|version| {
                    version
                        != current
                            .organization
                            .state()
                            .settings
                            .worktree_location
                            .version
                }) {
                    return Err(failure(ServerFailureClass::StaleAuthority));
                }
            }
            ConfirmedGitAction::Remove(scope) => {
                let host = self
                    .config
                    .core
                    .reducer
                    .git_host()
                    .ok_or_else(|| failure(ServerFailureClass::NativeUnavailable))?;
                let target = &scope.facts().target.directory;
                let pinned = current
                    .organization
                    .state()
                    .workspaces
                    .iter()
                    .filter_map(|workspace| {
                        workspace.pinned_directory.value.as_ref().map(|directory| {
                            PinDirectoryContainment {
                                workspace: workspace.id,
                                directory: directory.clone(),
                                version: workspace.pinned_directory.version,
                                containment: host.containment(directory, target),
                            }
                        })
                    })
                    .collect();
                let panes = current
                    .organization
                    .state()
                    .panes
                    .iter()
                    .map(|pane| {
                        let directory = current
                            .runtime
                            .panes
                            .iter()
                            .find(|v| v.pane == pane.id)
                            .and_then(|v| v.confirmed_directory.as_ref())
                            .map(|v| v.directory())
                            .unwrap_or_else(|| pane.restoration_directory());
                        PaneDirectoryContainment {
                            pane: pane.id,
                            directory: directory.clone(),
                            containment: host.containment(directory, target),
                        }
                    })
                    .collect();
                let mut fallbacks = Vec::new();
                for plan in &scope.retained().parts().fallbacks {
                    let record = current
                        .organization
                        .state()
                        .worktrees
                        .iter()
                        .find(|v| v.id == plan.worktree)
                        .ok_or_else(|| failure(ServerFailureClass::StaleAuthority))?;
                    if !fallbacks
                        .iter()
                        .any(|v: &FallbackWorktree| v.record.id == record.id)
                    {
                        fallbacks.push(FallbackWorktree { record: record.clone(), bare: false,
                            available: !record.missing && host.containment(&record.directory, &record.directory) == spaceterm_core::server_directories::DirectoryContainment::Inside,
                            containment: host.containment(&record.directory, target) });
                    }
                }
                revalidate_removal(
                    scope,
                    &current.organization,
                    &current.runtime,
                    &AffectedWorktreeWork {
                        pinned,
                        panes,
                        fallbacks,
                    },
                )
                .map_err(git_failure)?;
                let expected = scope.retained().parts();
                if expected.panes.iter().any(|pane| {
                    self.registry.as_ref().is_some_and(|registry| {
                        registry.sessions().iter().any(|session| {
                            session.pane == pane.hierarchy.pane && Some(*session) != pane.session
                        })
                    })
                }) {
                    return Err(failure(ServerFailureClass::Capacity));
                }
                if expected
                    .tabs
                    .iter()
                    .any(|v| !current.organization.state().tabs.contains(v))
                    || expected.membership_versions.iter().any(|v| {
                        !current
                            .organization
                            .state()
                            .retained
                            .as_ref()
                            .is_some_and(|facts| facts.tab_membership_versions().contains(v))
                    })
                    || expected.panes.iter().any(|pane| {
                        let runtime = current
                            .organization
                            .runtime()
                            .iter()
                            .find(|v| v.pane == pane.hierarchy.pane);
                        let stamp = current
                            .runtime
                            .panes
                            .iter()
                            .find(|v| v.pane == pane.hierarchy.pane);
                        runtime.is_none_or(|v| {
                            v.session_generation != pane.hierarchy.session_generation
                                || v.control_generation != pane.hierarchy.control_generation
                        }) || stamp.is_none_or(|v| {
                            v.session != pane.session
                                || v.context_generation != pane.hierarchy.context_generation
                                || v.work_generation != pane.work_generation
                                || v.startup_reserved != pane.startup_reserved
                        })
                    })
                {
                    return Err(failure(ServerFailureClass::StaleAuthority));
                }
            }
        }
        Ok(())
    }
    fn git_barrier(&mut self, intent: GitWriteIntent, reply: GitReply<()>) {
        let prepared = (|| {
            self.available()?;
            if !self.ordinary_open()
                || self.terminal_barrier
                || self.git_intents.get(&intent.binding().id) != Some(&intent)
                || intent.phase() != GitIntentPhase::Prepared
            {
                return Err(failure(ServerFailureClass::StaleAuthority));
            }
            let plan = self
                .git
                .leases
                .get(&intent.binding().id)
                .and_then(|v| v.plan.as_ref())
                .ok_or_else(|| failure(ServerFailureClass::StaleAuthority))?;
            self.git_revalidate(plan)?;
            if intent
                .parts()
                .affected
                .parts()
                .panes
                .iter()
                .any(|v| v.startup_reserved)
            {
                return Err(failure(ServerFailureClass::StaleAuthority));
            }
            let sessions: Vec<_> = intent
                .parts()
                .affected
                .parts()
                .panes
                .iter()
                .filter_map(|v| v.session)
                .collect();
            if !sessions.is_empty() {
                self.git
                    .leases
                    .get_mut(&intent.binding().id)
                    .ok_or_else(|| failure(ServerFailureClass::StaleAuthority))?
                    .barrier = GitBarrier::Paused;
            }
            let mut tasks = Vec::new();
            for session in sessions {
                self.registry()?.pause_pane(session.pane);
                if let Some(authority) = self.registry()?.authority(session) {
                    tasks.push(self.registry()?.transition(session, neutral(authority)?)?);
                } else if self.starting.contains_key(&session) {
                    return Err(failure(ServerFailureClass::StaleAuthority));
                }
            }
            Ok(tasks)
        })();
        let tasks = match prepared {
            Ok(tasks) => tasks,
            Err(error) => {
                let _ = reply.send(Err(error));
                return;
            }
        };
        self.busy = true;
        let events = self.events.clone();
        let inspect = self.registry.as_ref().map(|v| v.inspect());
        tokio::spawn(async move {
            let result = async {
                let mut applied = Vec::new();
                for task in tasks {
                    applied.push(
                        task.await
                            .map_err(|_| failure(ServerFailureClass::RuntimeUnavailable))??,
                    );
                }
                if let Some(inspect) = inspect {
                    inspect.await?;
                }
                Ok(applied)
            }
            .await;
            let _ = events
                .send(Event::GitBarrier {
                    intent,
                    reply,
                    result,
                })
                .await;
        });
    }
    pub(super) fn git_barrier_finished(
        &mut self,
        intent: GitWriteIntent,
        reply: GitReply<()>,
        result: Result<Vec<AuthorityApplied>, ServerFailure>,
    ) {
        self.busy = false;
        let result = (|| {
            if *self.phase.borrow() == ServicePhase::RecoveryRequired {
                return Err(failure(ServerFailureClass::RecoveryRequired));
            }
            for applied in result? {
                self.registry()?.authority_applied(applied)?;
                self.registry()?
                    .pause_pane(applied.transition().next().session().pane);
            }
            let plan = self
                .git
                .leases
                .get(&intent.binding().id)
                .and_then(|v| v.plan.as_ref())
                .ok_or_else(|| failure(ServerFailureClass::StaleAuthority))?;
            self.git_revalidate(plan)?;
            self.git
                .leases
                .get_mut(&intent.binding().id)
                .ok_or_else(|| failure(ServerFailureClass::StaleAuthority))?
                .barrier = GitBarrier::Checked;
            Ok(())
        })();
        let _ = reply.send(result);
    }
    fn git_retire(&mut self, intent: GitWriteIntent, reply: GitReply<CommitReceipt>) {
        if let Some(descriptor) = self.retirements.get(&intent.binding().id)
            && descriptor.binding() == intent.binding()
            && self.git_intents.get(&intent.binding().id) == Some(&intent)
            && let Some(reference) = self.git_references.get(&intent.binding().id)
        {
            let _ = reply.send(Ok(CommitReceipt {
                store_revision: reference.recorded_at(),
                organization_revision: descriptor.acceptance_revision(),
            }));
            return;
        }
        let prepared = (|| {
            self.available()?;
            if self.git_intents.get(&intent.binding().id) != Some(&intent)
                || intent.phase() != GitIntentPhase::Started
            {
                return Err(failure(ServerFailureClass::StaleAuthority));
            }
            let reduction = self
                .config
                .core
                .reducer
                .prepare_git_retirement(&intent, self.cursor)?;
            let descriptor = reduction
                .retirement
                .clone()
                .ok_or_else(|| failure(ServerFailureClass::RecoveryRequired))?;
            let mut parts = self.cursor.parts().map_err(store_failure)?;
            reduction.append_to(&mut parts)?;
            let basis = OperationReductionBasis::new(OperationReductionBasisParts {
                binding: intent.binding(),
                base: self.cursor.organization,
                resulting: parts.resulting_organization_revision,
            })
            .map_err(ledger_failure)?;
            parts
                .operation_writes
                .push(OperationWrite::ReductionBasis(basis));
            let context = TransactionContext::new(
                self.cursor.store,
                self.cursor.organization,
                parts.resulting_organization_revision,
                Some(basis),
            )
            .map_err(ledger_failure)?;
            let reference = self
                .git_references
                .get(&intent.binding().id)
                .copied()
                .ok_or_else(|| failure(ServerFailureClass::RecoveryRequired))?;
            self.config
                .core
                .ledger
                .prepare_external_progress(
                    intent.binding(),
                    progress(reference, Some(descriptor.clone()))?,
                    context,
                )?
                .append_to(&mut parts)
                .map_err(ledger_failure)?;
            Ok((parts, Arc::new(descriptor)))
        })();
        match prepared {
            Ok((parts, descriptor)) => self.commit(
                parts,
                CommitAction::Auxiliary {
                    writes: vec![],
                    retirement: Some(descriptor),
                    reply,
                },
            ),
            Err(error) => {
                self.abort_preparation();
                let _ = reply.send(Err(error));
            }
        }
    }
    fn git_complete(
        &mut self,
        completion: GitCompletionPlan,
        reply: GitReply<GitCompletionCommit>,
    ) {
        let binding = completion.intent.binding();
        if let Ok(OperationResponse::Completed {
            outcome: Outcome::Git(outcome),
            ..
        }) = self.lookup(binding)
        {
            let result = self
                .config
                .core
                .ledger
                .completion_receipt(binding)
                .and_then(|receipt| {
                    receipt.ok_or_else(|| failure(ServerFailureClass::RecoveryRequired))
                })
                .map(|receipt| GitCompletionCommit::durable(receipt, outcome));
            let _ = reply.send(result);
            return;
        }
        if let Err(error) = self.available() {
            let _ = reply.send(Err(error));
            return;
        }
        let Some(installed) = self.git_intents.get(&binding.id).cloned() else {
            // Prepared persistence can fail definitely after allocation. No native
            // work ran; settle its original AwaitingIntent reservation only.
            if completion.intent.phase() == GitIntentPhase::Prepared
                && completion.external.disposition == ExternalDisposition::NoEffectEstablished
            {
                self.git_complete_reduction(completion, reply, false);
            } else {
                let _ = reply.send(Err(failure(ServerFailureClass::StaleAuthority)));
            }
            return;
        };
        if installed.phase() == GitIntentPhase::Prepared {
            if completion.external.disposition != ExternalDisposition::NoEffectEstablished
                || completion.external.failure.is_none()
            {
                let _ = reply.send(Err(failure(ServerFailureClass::StaleAuthority)));
                return;
            }
            self.git_complete_reduction(completion, reply, true);
            return;
        }
        if installed.phase() == GitIntentPhase::ExternalFinished {
            if installed.parts().external_result.as_ref() != Some(&completion.external) {
                let _ = reply.send(Err(failure(ServerFailureClass::StaleAuthority)));
                return;
            }
            let mut completion = completion;
            completion.intent = installed;
            self.git_complete_reduction(completion, reply, true);
            return;
        }
        let prepared = (|| {
            if installed != completion.intent {
                return Err(failure(ServerFailureClass::StaleAuthority));
            }
            let mut parts = self.cursor.parts().map_err(store_failure)?;
            parts
                .operation_writes
                .push(OperationWrite::GitIntent(GitIntentMutation::Advance {
                    binding,
                    expected_phase: GitIntentPhase::Started,
                    next: GitIntentPhase::ExternalFinished,
                    result: Some(completion.external.clone()),
                }));
            let reference = ExternalIntentReference::new(ExternalIntentReferenceParts {
                binding,
                scope: ExternalIntentScope::Git(installed.parts().repository.scope()),
                phase: ExternalIntentPhase::ExternalFinished,
                recorded_at: parts.resulting_store_revision,
            })
            .map_err(ledger_failure)?;
            let retirement = self
                .retirements
                .get(&binding.id)
                .map(|v| v.as_ref().clone());
            self.config
                .core
                .ledger
                .prepare_external_progress(
                    binding,
                    progress(reference, retirement)?,
                    self.cursor.context().map_err(ledger_failure)?,
                )?
                .append_to(&mut parts)
                .map_err(ledger_failure)?;
            Ok(parts)
        })();
        match prepared {
            Ok(parts) => self.commit(
                parts,
                CommitAction::Git(GitCommit::ExternalFinished(Box::new(completion), reply)),
            ),
            Err(error) => {
                self.abort_preparation();
                let _ = reply.send(Err(error));
            }
        }
    }
    fn git_complete_reduction(
        &mut self,
        completion: GitCompletionPlan,
        reply: GitReply<GitCompletionCommit>,
        recorded: bool,
    ) {
        let prepared = (|| {
            let revoked: Vec<_> = self.revoked.iter().copied().collect();
            let mut reduction = self.config.core.reducer.prepare(
                ReductionRequest::SettleStartedGit {
                    intent: &completion.intent,
                    result: &completion.external,
                    revoked_clients: &revoked,
                },
                self.cursor,
            )?;
            if !recorded {
                reduction
                    .auxiliary_writes
                    .retain(|v| !matches!(v, OperationWrite::GitIntent(_)));
            }
            if let Some(observation) = completion.recovery {
                reduction.outcome = Outcome::Git(
                    GitMutationOutcome::new(GitMutationOutcomeParts::Recovered {
                        server: self.cursor.server,
                        observation,
                        revision: (reduction.snapshot.state().revision != self.cursor.organization)
                            .then_some(reduction.snapshot.state().revision),
                    })
                    .map_err(git_failure)?,
                );
            }
            Ok(reduction)
        })();
        match prepared {
            Ok(reduction) => {
                let parts = self.completion_parts(
                    completion.intent.binding(),
                    &reduction,
                    reduction.snapshot.state().revision,
                );
                match (parts, reduction.outcome) {
                    (Ok(parts), Outcome::Git(outcome)) => self.commit(
                        parts,
                        CommitAction::Git(GitCommit::Completed(outcome, reply)),
                    ),
                    (Err(error), _) => {
                        self.abort_preparation();
                        let _ = reply.send(Err(error));
                    }
                    _ => {
                        self.abort_preparation();
                        let _ = reply.send(Err(failure(ServerFailureClass::RecoveryRequired)));
                    }
                }
            }
            Err(error) => {
                self.abort_preparation();
                let _ = reply.send(Err(error));
            }
        }
    }
    pub(super) fn git_committed(
        &mut self,
        action: GitCommit,
        writes: &[OperationWrite],
        receipt: CommitReceipt,
    ) {
        self.apply_git_writes(writes);
        match action {
            GitCommit::Inventory(lease, reply) => {
                let result = lease
                    .as_ref()
                    .map_or(Ok(()), |v| v.retain_committed().map_err(git_failure))
                    .and_then(|_| self.git_snapshot());
                let _ = reply.send(result);
            }
            GitCommit::Cancelled(reply) => {
                let _ = reply.send(Ok(()));
            }
            GitCommit::Admitted(binding, reply) => {
                let _ = reply.send(Ok(binding));
            }
            GitCommit::Completed(outcome, reply) => {
                let _ = reply.send(Ok(GitCompletionCommit::durable(receipt, outcome)));
            }
            GitCommit::ExternalFinished(mut completion, reply) => {
                if let Some(intent) = self.git_intents.get(&completion.intent.binding().id) {
                    completion.intent = intent.clone();
                }
                self.git_complete(*completion, reply);
            }
        }
    }
    fn git_release(&mut self, binding: OperationBinding) -> Result<(), ServerFailure> {
        if !matches!(
            self.config.core.ledger.lookup(binding)?,
            OperationLookup::Completed { .. } | OperationLookup::AcknowledgedRetired
        ) {
            return Err(failure(ServerFailureClass::StaleAuthority));
        }
        let Some(lease) = self.git.leases.remove(&binding.id) else {
            return Ok(());
        };
        if lease.barrier == GitBarrier::Open {
            return Ok(());
        }
        let snapshot = self.config.core.reducer.snapshot();
        let owners = self
            .registry
            .as_ref()
            .ok_or_else(|| failure(ServerFailureClass::RecoveryRequired))?
            .sessions();
        let mut clients = BTreeSet::new();
        for pane in &lease.intent.parts().affected.parts().panes {
            if let Some(session) = pane.session
                && !self.registry()?.is_retiring(session)
                && let Some(record) = snapshot
                    .runtime()
                    .iter()
                    .find(|v| v.pane == session.pane && v.session_generation == session.generation)
                && let ControlRecord::Held { client, .. } = record.control
            {
                clients.insert(client);
            }
        }
        // A failed pre-Started commit never reinstalls the old context. Rotate
        // every surviving controller's context past its neutral worker fence.
        for client in &clients {
            let mut context = self
                .connections
                .get(client)
                .map(|v| v.context)
                .ok_or_else(|| failure(ServerFailureClass::StaleAuthority))?;
            for session in &owners {
                if snapshot.runtime().iter().any(|v| v.pane == session.pane && matches!(v.control, ControlRecord::Held { client: owner, .. } if owner == *client))
                    && let Some(authority) = self.registry()?.authority(*session) { context = context.max(authority.parts().context); }
            }
            let connection = self
                .connections
                .get_mut(client)
                .ok_or_else(|| failure(ServerFailureClass::StaleAuthority))?;
            connection.context = context
                .checked_successor()
                .ok_or_else(|| failure(ServerFailureClass::Exhausted))?;
            connection.focus = None;
        }
        for session in owners {
            if !self.registry()?.is_retiring(session)
                && snapshot.runtime().iter().any(|v| v.pane == session.pane && v.session_generation == session.generation && matches!(v.control, ControlRecord::Held { client, .. } if clients.contains(&client)))
                && !self.git.leases.values().any(|v| v.intent.parts().affected.parts().panes.iter().any(|p| p.hierarchy.pane == session.pane)) {
                self.authority_task(session, self.current_authority(session)?, None, None);
            }
        }
        Ok(())
    }
    fn git_recover(&mut self, intent: GitWriteIntent) -> Result<GitReservation, ServerFailure> {
        if self.git_intents.get(&intent.binding().id) != Some(&intent) {
            return Err(failure(ServerFailureClass::StaleAuthority));
        }
        let allocation = self
            .git_allocations
            .get(&intent.binding().id)
            .ok_or_else(|| failure(ServerFailureClass::RecoveryRequired))?
            .clone();
        let reservation =
            GitReservation::new(intent.clone(), allocation.clone()).map_err(git_failure)?;
        self.git.leases.insert(
            intent.binding().id,
            GitLease {
                plan: None,
                intent,
                barrier: GitBarrier::Checked,
            },
        );
        Ok(reservation)
    }
    pub(super) fn git_started_allowed(&self, binding: OperationBinding) -> bool {
        self.git.leases.get(&binding.id).is_some_and(|lease| {
            lease.intent.binding() == binding && lease.barrier == GitBarrier::Checked
        })
    }
    pub(super) fn git_conflicts(&self, command: &Command) -> bool {
        self.git.leases.values().any(|lease| {
            let scope = lease.intent.parts().affected.parts();
            match command {
                Command::CompareAcquire(request) => scope.panes.iter().any(|p| p.hierarchy.pane == request.pane),
                Command::TakeOver(request) => scope.panes.iter().any(|p| p.hierarchy.pane == request.pane),
                Command::ReleaseControl(request) => scope.panes.iter().any(|p| p.hierarchy.pane == request.grant.pane()),
                Command::BeginStartup(intent) => scope.panes.iter().any(|p| p.hierarchy.pane == intent.fence().session().pane),
                Command::CreatePane { tab, .. } => scope.tabs.iter().any(|v| v.id == *tab),
                Command::CreateTab { workspace, source } => scope.tabs.iter().any(|v| v.workspace == *workspace)
                    || matches!(source, TabCreationSource::Worktree(worktree) if *worktree == lease.intent.parts().action.worktree()),
                Command::RepairPaneDirectory { pane, .. } => scope.panes.iter().any(|v| v.hierarchy.pane == *pane),
                Command::SetPinnedDirectory { workspace, .. } => scope.tabs.iter().any(|v| v.workspace == *workspace),
                Command::CompleteStartup(measurement) => scope.panes.iter().any(|p| p.hierarchy.pane == measurement.fence.session().pane),
                Command::CompleteMeasuredStartup(measurement) => scope.panes.iter().any(|p| p.hierarchy.pane == measurement.fence.session().pane),
                Command::CloseConfirmed { target, .. } => match target {
                    CloseTarget::Pane(id) => scope.panes.iter().any(|v| v.hierarchy.pane == *id),
                    CloseTarget::Tab(id) => scope.tabs.iter().any(|v| v.id == *id),
                    CloseTarget::Workspace(id) => scope.tabs.iter().any(|v| v.workspace == *id),
                    CloseTarget::WorktreeTabs { workspace, worktree } => scope.tabs.iter().any(|v| v.workspace == *workspace && v.worktree == Some(*worktree)),
                },
                Command::KillWorkspaceConfirmed { workspace, .. } => scope.tabs.iter().any(|v| v.workspace == *workspace),
                _ => false,
            }
        })
    }
}
fn progress(
    reference: ExternalIntentReference,
    retirement: Option<RetirementDescriptor>,
) -> Result<PendingExternalProgress, ServerFailure> {
    PendingExternalProgress::new(PendingExternalProgressParts {
        binding: reference.binding(),
        effect: ExternalEffectKind::GitWrite,
        state: ExternalProgressState::RecordedIntent {
            reference,
            retirement: retirement.map(Box::new),
        },
    })
    .map_err(ledger_failure)
}
pub(crate) fn git_failure(error: GitFailure) -> ServerFailure {
    failure(match error {
        GitFailure::WrongServer => ServerFailureClass::ScopeMismatch,
        GitFailure::Unauthorized => ServerFailureClass::Authentication,
        GitFailure::Capacity | GitFailure::Busy => ServerFailureClass::Capacity,
        GitFailure::IndeterminateDurability => ServerFailureClass::RecoveryRequired,
        _ => ServerFailureClass::StaleAuthority,
    })
}
pub(super) fn reject(request: GitRequest, error: ServerFailure) {
    match request {
        GitRequest::Snapshot(reply)
        | GitRequest::Inventory(_, _, reply)
        | GitRequest::Outside(_, _, reply) => {
            let _ = reply.send(Err(error));
        }
        GitRequest::Authorize(_, reply)
        | GitRequest::Barrier(_, reply)
        | GitRequest::Release(_, reply)
        | GitRequest::Cancel(_, _, reply)
        | GitRequest::Close(reply) => {
            let _ = reply.send(Err(error));
        }
        GitRequest::Lookup(_, _, _, reply) => {
            let _ = reply.send(Err(error));
        }
        GitRequest::Admit(_, reply) => {
            let _ = reply.send(Err(error));
        }
        GitRequest::Allocate(_, _, reply) => {
            let _ = reply.send(Err(error));
        }
        GitRequest::Reserve(_, _, reply) | GitRequest::Recover(_, reply) => {
            let _ = reply.send(Err(error));
        }
        GitRequest::Prepared(_, reply)
        | GitRequest::Started(_, reply)
        | GitRequest::Retire(_, reply) => {
            let _ = reply.send(Err(error));
        }
        GitRequest::Complete(_, reply) => {
            let _ = reply.send(Err(error));
        }
    }
}
pub(super) fn reject_commit(action: GitCommit, error: ServerFailure) {
    match action {
        GitCommit::Inventory(_, reply) => {
            let _ = reply.send(Err(error));
        }
        GitCommit::Cancelled(reply) => {
            let _ = reply.send(Err(error));
        }
        GitCommit::Admitted(_, reply) => {
            let _ = reply.send(Err(error));
        }
        GitCommit::ExternalFinished(_, reply) | GitCommit::Completed(_, reply) => {
            let _ = reply.send(Err(error));
        }
    }
}

impl Actor {
    pub(super) fn git_observe_writes(&mut self, writes: &[OperationWrite]) {
        for write in writes {
            match write {
                OperationWrite::GitRepository(GitRepositoryMutation::Put(value)) => {
                    self.git_repositories.retain(|v| v.scope() != value.scope());
                    self.git_repositories.push(value.clone());
                }
                OperationWrite::GitRepository(GitRepositoryMutation::Delete(id)) => self
                    .git_repositories
                    .retain(|v| v.scope().repository != *id),
                OperationWrite::GitAllocation(GitAllocationMutation::Reserve(value)) => {
                    self.git_allocations
                        .insert(value.binding().id, value.as_ref().clone());
                }
                OperationWrite::GitAllocation(
                    GitAllocationMutation::Consume { binding }
                    | GitAllocationMutation::Abandon { binding },
                ) => {
                    self.git_allocations.remove(&binding.id);
                }
                OperationWrite::Ledger(write) => match write.as_ref() {
                    LedgerWrite::RecordExternalProgress { record, .. } => {
                        if let Some(progress) = &record.external_progress
                            && let ExternalProgressState::RecordedIntent { reference, .. } =
                                progress.state()
                        {
                            self.git_references.insert(record.binding.id, *reference);
                        }
                    }
                    LedgerWrite::CompleteOperation { record, .. } => {
                        self.git_references.remove(&record.binding.id);
                    }
                    _ => {}
                },
                _ => {}
            }
        }
    }
}
impl Actor {
    fn git_prepared(&mut self, reservation: GitReservation, reply: GitReply<CommitReceipt>) {
        let result = (|| {
            self.available()?;
            let intent = reservation.intent();
            if self
                .git
                .leases
                .get(&intent.binding().id)
                .is_none_or(|v| &v.intent != intent)
                || self.git_allocations.get(&intent.binding().id) != Some(reservation.allocation())
                || self.git_intents.contains_key(&intent.binding().id)
            {
                return Err(failure(ServerFailureClass::StaleAuthority));
            }
            let reference = ExternalIntentReference::new(ExternalIntentReferenceParts {
                binding: intent.binding(),
                scope: ExternalIntentScope::Git(intent.parts().repository.scope()),
                phase: ExternalIntentPhase::Prepared,
                recorded_at: self
                    .cursor
                    .store
                    .checked_successor()
                    .ok_or_else(|| failure(ServerFailureClass::Exhausted))?,
            })
            .map_err(ledger_failure)?;
            Ok((
                progress(reference, None)?,
                vec![
                    OperationWrite::GitRepository(GitRepositoryMutation::Put(
                        intent.parts().repository.clone(),
                    )),
                    OperationWrite::GitIntent(GitIntentMutation::Insert(Box::new(intent.clone()))),
                ],
            ))
        })();
        match result {
            Ok((progress, writes)) => {
                self.external_progress(reservation.binding(), progress, writes, reply)
            }
            Err(error) => {
                let _ = reply.send(Err(error));
            }
        }
    }
    fn git_started(&mut self, intent: GitWriteIntent, reply: GitReply<CommitReceipt>) {
        let prepared = (|| {
            self.available()?;
            if !self.ordinary_open()
                || self.terminal_barrier
                || self
                    .revoked
                    .contains(&intent.binding().id.stream().client())
                || !self.git_started_allowed(intent.binding())
                || self.git_intents.get(&intent.binding().id) != Some(&intent)
            {
                return Err(failure(ServerFailureClass::StaleAuthority));
            }
            let plan = self
                .git
                .leases
                .get(&intent.binding().id)
                .and_then(|v| v.plan.as_ref())
                .ok_or_else(|| failure(ServerFailureClass::StaleAuthority))?;
            self.git_revalidate(plan)?;
            let mut next = intent.parts().clone();
            next.phase = GitIntentPhase::Started;
            let next = GitWriteIntent::new(next).map_err(git_failure)?;
            let mut parts = self.cursor.parts().map_err(store_failure)?;
            let writes = vec![OperationWrite::GitIntent(GitIntentMutation::Advance {
                binding: intent.binding(),
                expected_phase: GitIntentPhase::Prepared,
                next: GitIntentPhase::Started,
                result: None,
            })];
            parts.operation_writes.extend(writes.iter().cloned());
            // Started and its non-deleting retirement share one prerequisite.
            // EOF and a later End All cannot slip between the two durable facts.
            // Native termination remains strictly after this physical commit.
            let retirement = if next.parts().affected.parts().panes.is_empty() {
                None
            } else {
                let reduction = self
                    .config
                    .core
                    .reducer
                    .prepare_git_retirement(&next, self.cursor)?;
                let descriptor = reduction
                    .retirement
                    .clone()
                    .ok_or_else(|| failure(ServerFailureClass::RecoveryRequired))?;
                reduction.append_to(&mut parts)?;
                Some(Arc::new(descriptor))
            };
            let basis = retirement
                .as_ref()
                .map(|_| {
                    OperationReductionBasis::new(OperationReductionBasisParts {
                        binding: intent.binding(),
                        base: self.cursor.organization,
                        resulting: parts.resulting_organization_revision,
                    })
                })
                .transpose()
                .map_err(ledger_failure)?;
            if let Some(basis) = basis {
                parts
                    .operation_writes
                    .push(OperationWrite::ReductionBasis(basis));
            }
            let reference = ExternalIntentReference::new(ExternalIntentReferenceParts {
                binding: intent.binding(),
                scope: ExternalIntentScope::Git(intent.parts().repository.scope()),
                phase: ExternalIntentPhase::Started,
                recorded_at: parts.resulting_store_revision,
            })
            .map_err(ledger_failure)?;
            let context = TransactionContext::new(
                self.cursor.store,
                self.cursor.organization,
                parts.resulting_organization_revision,
                basis,
            )
            .map_err(ledger_failure)?;
            self.config
                .core
                .ledger
                .prepare_external_progress(
                    intent.binding(),
                    progress(reference, retirement.as_ref().map(|v| v.as_ref().clone()))?,
                    context,
                )?
                .append_to(&mut parts)
                .map_err(ledger_failure)?;
            Ok((parts, writes, retirement))
        })();
        match prepared {
            Ok((parts, writes, retirement)) => self.commit(
                parts,
                CommitAction::Auxiliary {
                    writes,
                    retirement,
                    reply,
                },
            ),
            Err(error) => {
                self.abort_preparation();
                let _ = reply.send(Err(error));
            }
        }
    }
}
impl Actor {
    fn git_inventory_basis(&self, basis: &GitAuthoritySnapshot) -> Result<(), ServerFailure> {
        let current = self.git_snapshot()?;
        let old = basis.organization.state();
        let now = current.organization.state();
        if basis.organization.epoch() != self.cursor.epoch
            || old.server != self.cursor.server
            || basis.repositories != current.repositories
            || old.panes != now.panes
            || old.tabs != now.tabs
            || old.workspaces.iter().any(|workspace| {
                now.workspaces
                    .iter()
                    .find(|v| v.id == workspace.id)
                    .is_none_or(|v| {
                        v.root_tab != workspace.root_tab
                            || v.pinned_directory != workspace.pinned_directory
                    })
            })
            || basis.organization.runtime().iter().any(|runtime| {
                current
                    .organization
                    .runtime()
                    .iter()
                    .find(|v| v.pane == runtime.pane)
                    .is_none_or(|v| v.session_generation != runtime.session_generation)
            })
            || old.retained.as_ref().map(|v| {
                (
                    v.repository_inventory_versions(),
                    v.tab_membership_versions(),
                )
            }) != now.retained.as_ref().map(|v| {
                (
                    v.repository_inventory_versions(),
                    v.tab_membership_versions(),
                )
            })
        {
            return Err(failure(ServerFailureClass::StaleAuthority));
        }
        Ok(())
    }
    fn git_inventory(
        &mut self,
        principal: Option<AuthenticatedGitPrincipal>,
        request: GitInventoryReconciliation,
        reply: GitReply<GitAuthoritySnapshot>,
    ) {
        let result = (|| {
            self.available()?;
            if let Some(principal) = principal {
                self.git_principal(principal)?;
            }
            if !self.ordinary_open() || self.terminal_barrier {
                return Err(failure(ServerFailureClass::Capacity));
            }
            self.git_inventory_basis(&request.basis)?;
            let inventory = &request.inventory;
            if inventory.records.is_empty()
                || inventory.records.len() > 4096
                || inventory
                    .current
                    .is_some_and(|v| v >= inventory.records.len())
                || inventory.main_root.server() != self.cursor.server
                || inventory.common_directory.server() != self.cursor.server
                || self.git.leases.values().any(|v| {
                    v.intent.parts().repository.parts().common_directory
                        == inventory.common_directory
                })
                || self.git_intents.values().any(|v| {
                    v.parts().repository.parts().common_directory == inventory.common_directory
                })
            {
                return Err(failure(ServerFailureClass::Capacity));
            }
            if let Some(lease) = &inventory.lease {
                lease.revalidate().map_err(git_failure)?;
            }
            let reduction = self
                .config
                .core
                .reducer
                .prepare_git_inventory(&request, self.cursor)?;
            let mut parts = self.cursor.parts().map_err(store_failure)?;
            reduction.append_to(&mut parts)?;
            Ok(parts)
        })();
        match result {
            Ok(parts) => self.commit(
                parts,
                CommitAction::Git(GitCommit::Inventory(request.inventory.lease, reply)),
            ),
            Err(error) => {
                self.abort_preparation();
                let _ = reply.send(Err(error));
            }
        }
    }
    fn git_outside(
        &mut self,
        principal: AuthenticatedGitPrincipal,
        request: GitOutsideSourceReconciliation,
        reply: GitReply<GitAuthoritySnapshot>,
    ) {
        let result = (|| {
            self.available()?;
            self.git_principal(principal)?;
            self.git_inventory_basis(&request.basis)?;
            if !self.ordinary_open() || self.terminal_barrier || !self.git.leases.is_empty() {
                return Err(failure(ServerFailureClass::Capacity));
            }
            for edit in &request.memberships {
                let state = request.basis.organization.state();
                let tab = state
                    .tabs
                    .iter()
                    .find(|v| v.id == edit.tab)
                    .ok_or_else(|| failure(ServerFailureClass::StaleAuthority))?;
                let workspace = state
                    .workspaces
                    .iter()
                    .find(|v| v.id == tab.workspace)
                    .ok_or_else(|| failure(ServerFailureClass::StaleAuthority))?;
                let pane = state
                    .panes
                    .iter()
                    .find(|v| v.id == tab.root_pane)
                    .ok_or_else(|| failure(ServerFailureClass::StaleAuthority))?;
                if edit.worktree.is_some()
                    || workspace.pinned_directory.value.is_some()
                    || workspace.root_tab != tab.id
                    || pane
                        .confirmed_restoration_directory
                        .value
                        .as_ref()
                        .unwrap_or(&pane.starting_directory)
                        != &request.directory
                {
                    return Err(failure(ServerFailureClass::StaleAuthority));
                }
            }
            let reduction = self
                .config
                .core
                .reducer
                .prepare_git_outside_source(&request, self.cursor)?;
            let mut parts = self.cursor.parts().map_err(store_failure)?;
            reduction.append_to(&mut parts)?;
            Ok(parts)
        })();
        match result {
            Ok(parts) => self.commit(parts, CommitAction::Git(GitCommit::Inventory(None, reply))),
            Err(error) => {
                self.abort_preparation();
                let _ = reply.send(Err(error));
            }
        }
    }
}

impl Actor {
    pub(super) fn git_directory_task(
        &self,
        task: ServerFuture<
            'static,
            Result<spaceterm_core::reducer::DirectoryValidation, ServerFailure>,
        >,
        command: &Command,
    ) -> ServerFuture<'static, Result<spaceterm_core::reducer::DirectoryValidation, ServerFailure>>
    {
        let roots: Vec<_> = self
            .git
            .leases
            .values()
            .filter_map(|lease| match &lease.intent.parts().action {
                RetainedGitAction::Remove { directory, .. } => Some(directory.clone()),
                _ => None,
            })
            .collect();
        let host = self.config.core.reducer.git_host();
        let home_selected = matches!(
            command,
            Command::CreateWorkspace {
                source: WorkspaceCreationSource::ServerHome { .. },
                ..
            }
        );
        let clears_pin = matches!(command, Command::SetPinnedDirectory { value: None, .. });
        Box::pin(async move {
            let facts = task.await?;
            if roots.is_empty() || clears_pin {
                return Ok(facts);
            }
            let host = host.ok_or_else(|| failure(ServerFailureClass::NativeUnavailable))?;
            let mut directories: Vec<_> =
                [&facts.requested, &facts.pin, &facts.source, &facts.worktree]
                    .into_iter()
                    .filter_map(|v| v.as_ref().map(|v| v.directory.clone()))
                    .collect();
            if home_selected {
                directories.push(facts.home.directory.clone());
            }
            tokio::task::spawn_blocking(move || {
                for directory in directories {
                    for root in &roots {
                        if host.containment(&directory, root)
                            != spaceterm_core::server_directories::DirectoryContainment::Outside
                        {
                            return Err(failure(ServerFailureClass::Capacity));
                        }
                    }
                }
                Ok(())
            })
            .await
            .map_err(|_| failure(ServerFailureClass::NativeUnavailable))??;
            Ok(facts)
        })
    }
}
impl Actor {
    fn git_cancel(&mut self, binding: OperationBinding, reason: GitFailure, reply: GitReply<()>) {
        let prepared = (|| {
            self.available()?;
            match self.config.core.ledger.lookup(binding)? {
                OperationLookup::Completed { .. } => return Ok(None),
                OperationLookup::Pending if !self.git_intents.contains_key(&binding.id) => {}
                _ => return Err(failure(ServerFailureClass::StaleAuthority)),
            }
            let reduction = self.config.core.reducer.prepare_git_cancellation(
                binding,
                reason,
                self.git_allocations.contains_key(&binding.id),
                self.cursor,
            )?;
            self.completion_parts(binding, &reduction, reduction.snapshot.state().revision)
                .map(Some)
        })();
        match prepared {
            Ok(Some(parts)) => self.commit(parts, CommitAction::Git(GitCommit::Cancelled(reply))),
            Ok(None) => {
                let _ = reply.send(Ok(()));
            }
            Err(error) => {
                self.abort_preparation();
                let _ = reply.send(Err(error));
            }
        }
    }
}
impl Actor {
    pub(super) fn advance_git_cleanup(
        &mut self,
        descriptor: Arc<RetirementDescriptor>,
    ) -> Result<bool, ServerFailure> {
        let Some(cleanup) = self
            .registry
            .as_ref()
            .ok_or_else(|| failure(ServerFailureClass::RecoveryRequired))?
            .cleanup_for(RetirementRequest::Operation(&descriptor))?
        else {
            return Ok(false);
        };
        let observed = observed_retirement(&descriptor, &cleanup)?;
        if observed != *descriptor {
            let reference = self
                .git_references
                .get(&descriptor.binding().id)
                .copied()
                .ok_or_else(|| failure(ServerFailureClass::RecoveryRequired))?;
            let mut parts = self.cursor.parts().map_err(store_failure)?;
            if let Some(reduction) = self
                .config
                .core
                .reducer
                .prepare_git_cleanup(&cleanup, self.cursor)?
            {
                reduction.append_to(&mut parts)?;
            }
            self.config
                .core
                .ledger
                .prepare_external_progress(
                    descriptor.binding(),
                    progress(reference, Some(observed.clone()))?,
                    self.cursor.context().map_err(ledger_failure)?,
                )?
                .append_to(&mut parts)
                .map_err(ledger_failure)?;
            self.commit(
                parts,
                CommitAction::RetirementObserved {
                    descriptor: Arc::new(observed),
                },
            );
            return Ok(true);
        }
        if let Some(waiters) = self.git_cleanup_waiters.remove(&descriptor.binding().id) {
            for reply in waiters {
                let result = if descriptor.cleanup_confirmed() {
                    Ok(cleanup.clone())
                } else {
                    Err(failure(ServerFailureClass::CleanupIncomplete))
                };
                let _ = reply.send(result);
            }
        }
        Ok(false)
    }
}
