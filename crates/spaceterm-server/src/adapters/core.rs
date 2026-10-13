//! Compose the actual reducer and canonical ledger with the serialized actor.
//! Candidates stay with their domain owners until a checked physical receipt.
//! Test implementations of these ports remain in integration-test support.

use crate::contracts::ServerFailure;
use spaceterm_core::{
    authority::*, control::MeasuredStartupReservation, control_policy::ControlTransition,
    identity::*, observations::ConfirmedDirectoryObservation, operation_ledger_contracts::*,
    operations::*, organization::*, retirement::*, runtime_authority::SessionTerminalResult,
    storage::*, worktrees::intent::*,
};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommitCursor {
    pub server: ServerId,
    pub store: StoreRevision,
    pub organization: OrganizationRevision,
    pub epoch: ServerEpoch,
    pub high_water: IdentityHighWater,
}
impl CommitCursor {
    pub fn from_recovered(value: &RecoveredState) -> Self {
        Self {
            server: value.organization.server,
            store: value.store_revision,
            organization: value.organization.revision,
            epoch: value.last_epoch,
            high_water: value.high_water,
        }
    }
    pub fn parts(self) -> Result<TransactionParts, StoreFailure> {
        Ok(TransactionParts {
            server: self.server,
            expected_store_revision: self.store,
            resulting_store_revision: self
                .store
                .checked_successor()
                .ok_or(StoreFailure::Exhausted)?,
            expected_organization_revision: self.organization,
            resulting_organization_revision: self.organization,
            expected_epoch: self.epoch,
            resulting_epoch: self.epoch,
            expected_high_water: self.high_water,
            resulting_high_water: self.high_water,
            organization_writes: Vec::new(),
            operation_writes: Vec::new(),
        })
    }
    pub fn context(self) -> Result<TransactionContext, LedgerFailure> {
        TransactionContext::new(self.store, self.organization, self.organization, None)
    }
    pub fn install(
        &mut self,
        parts: &TransactionParts,
        receipt: CommitReceipt,
    ) -> Result<(), ServerFailure> {
        if parts.server != self.server
            || parts.expected_store_revision != self.store
            || parts.expected_organization_revision != self.organization
            || parts.expected_epoch != self.epoch
            || parts.expected_high_water != self.high_water
            || receipt.store_revision != parts.resulting_store_revision
            || receipt.organization_revision != parts.resulting_organization_revision
        {
            return Err(crate::failure::failure(
                crate::contracts::ServerFailureClass::RecoveryRequired,
            ));
        }
        self.store = receipt.store_revision;
        self.organization = receipt.organization_revision;
        self.epoch = parts.resulting_epoch;
        self.high_water = parts.resulting_high_water;
        Ok(())
    }
}

/// A projection of the reducer's staged candidate, not another hierarchy owner.
/// Auxiliary writes remain core-owned OperationWrite values in the same commit.
pub struct PreparedReduction {
    pub snapshot: Arc<OrganizationSnapshot>,
    pub delta: Option<Arc<OrganizationDelta>>,
    pub high_water: IdentityHighWater,
    pub organization_writes: Vec<OrganizationWrite>,
    pub auxiliary_writes: Vec<OperationWrite>,
    pub outcome: Outcome,
    pub retirement: Option<RetirementDescriptor>,
    pub native_retirement: Option<NativeRetirementDescriptor>,
}
impl PreparedReduction {
    pub fn append_to(&self, parts: &mut TransactionParts) -> Result<(), ServerFailure> {
        if self.snapshot.state().server != parts.server
            || self.snapshot.epoch() != parts.resulting_epoch
            || !self.high_water.dominates(parts.expected_high_water)
            || self.snapshot.state().revision < parts.expected_organization_revision
            || (self.snapshot.state().revision != parts.expected_organization_revision
                && parts.expected_organization_revision.checked_successor()
                    != Some(self.snapshot.state().revision))
        {
            return Err(crate::failure::failure(
                crate::contracts::ServerFailureClass::RecoveryRequired,
            ));
        }
        parts.resulting_organization_revision = self.snapshot.state().revision;
        parts.resulting_high_water = self.high_water;
        parts
            .organization_writes
            .extend(self.organization_writes.iter().cloned());
        parts
            .operation_writes
            .extend(self.auxiliary_writes.iter().cloned());
        Ok(())
    }
}

pub enum ReductionRequest<'a> {
    Command(&'a CanonicalOperationRequest),
    ValidatedCommand {
        request: &'a CanonicalOperationRequest,
        directories: &'a spaceterm_core::reducer::DirectoryValidation,
    },
    Rejected {
        binding: OperationBinding,
        failure: OperationFailure,
    },
    Control {
        binding: OperationBinding,
        transition: &'a ControlTransition,
    },
    StartupPreparationFailed {
        binding: OperationBinding,
        intent: &'a spaceterm_core::control::StartupIntent,
        failure: OperationFailure,
    },
    StartupReserved {
        binding: OperationBinding,
        reservation: &'a MeasuredStartupReservation,
    },
    RuntimeStarted(SessionKey),
    RuntimeFinished(SessionTerminalResult),
    RuntimeCleanup(SessionCleanupObservation),
    UnstartedLaunchFailed {
        terminal: SessionTerminalResult,
        cleanup: SessionCleanupObservation,
    },
    RestorationDirectoryConfirmed(&'a ConfirmedDirectoryObservation),
    RetirementCompleted {
        descriptor: &'a RetirementDescriptor,
        cleanup: &'a [SessionCleanupObservation],
    },
    NativeRetirementAccepted {
        request: spaceterm_core::native_management::NativeManagementRequestRef,
        action: spaceterm_core::native_management::NativeRetirementAction,
        sessions: &'a [RetiredSession],
    },
    NativeRetirementObserved {
        descriptor: &'a NativeRetirementDescriptor,
        cleanup: &'a [SessionCleanupObservation],
    },
    NativeRetirementCompleted {
        descriptor: &'a NativeRetirementDescriptor,
        cleanup: &'a [SessionCleanupObservation],
    },
    ReleaseClients(&'a [ClientId]),
    /// Stage allocation and control release without activating any pairing or
    /// launching a process. The extension supplies its matching core facts later
    /// in this same actor-owned transaction.
    PairingCompletion {
        binding: OperationBinding,
        allocation: crate::extensions::PairingAllocationRequest,
        revoked_clients: &'a [ClientId],
    },
    /// Check the original Started intent against current hierarchy without
    /// deleting membership before the external Git result.
    StartedGitRetirement {
        intent: &'a GitWriteIntent,
        descriptor: &'a RetirementDescriptor,
    },
    ReserveGitAllocation {
        binding: OperationBinding,
        repository: bool,
        worktree: bool,
        fallbacks: &'a [WorkspaceFallbackPlan],
    },
    /// Consume only the original operation's allocation against current
    /// membership, release revoked grants in the same candidate, and never
    /// supply executable shell-start authority.
    SettleStartedGit {
        intent: &'a GitWriteIntent,
        result: &'a RetainedExternalResult,
        revoked_clients: &'a [ClientId],
    },
}

pub trait ReducerPort: Send {
    fn prepare_git_cleanup(
        &mut self,
        _cleanup: &[SessionCleanupObservation],
        _cursor: CommitCursor,
    ) -> Result<Option<PreparedReduction>, ServerFailure> {
        Ok(None)
    }
    fn prepare_git_cancellation(
        &mut self,
        _binding: OperationBinding,
        _failure: spaceterm_core::worktrees::GitFailure,
        _reserved: bool,
        _cursor: CommitCursor,
    ) -> Result<PreparedReduction, ServerFailure> {
        Err(recovery_failure())
    }
    fn prepare_git_inventory(
        &mut self,
        _request: &crate::git::authority::GitInventoryReconciliation,
        _cursor: CommitCursor,
    ) -> Result<PreparedReduction, ServerFailure> {
        Err(recovery_failure())
    }
    fn prepare_git_outside_source(
        &mut self,
        _request: &crate::git::authority::GitOutsideSourceReconciliation,
        _cursor: CommitCursor,
    ) -> Result<PreparedReduction, ServerFailure> {
        Err(recovery_failure())
    }

    /// Generate the exact descriptor from the real installed reducer. Supplied
    /// descriptor comparison remains the existing generic extension contract.
    fn prepare_git_retirement(
        &mut self,
        _intent: &GitWriteIntent,
        _cursor: CommitCursor,
    ) -> Result<PreparedReduction, ServerFailure> {
        Err(crate::failure::failure(
            crate::contracts::ServerFailureClass::NativeUnavailable,
        ))
    }
    fn git_host(&self) -> Option<Arc<dyn crate::git::host::ServerGitHost>> {
        None
    }
    fn git_home(&self) -> Option<ServerDirectory> {
        None
    }
    /// Native directory observations run outside the serialized actor. The exact
    /// basis is compared again by the real reducer before the candidate commits.
    fn prepare_directories(
        &self,
        _request: &CanonicalOperationRequest,
    ) -> Result<
        Option<
            crate::contracts::ServerFuture<
                'static,
                Result<spaceterm_core::reducer::DirectoryValidation, ServerFailure>,
            >,
        >,
        ServerFailure,
    > {
        Ok(None)
    }

    fn recover(
        &mut self,
        recovered: &RecoveredState,
        epoch: ServerEpoch,
        cleanup: &[SessionCleanupObservation],
    ) -> Result<PreparedReduction, ServerFailure>;
    fn snapshot(&self) -> Arc<OrganizationSnapshot>;
    /// Retain the producer's original stamp and tick. Assessment policy, including
    /// stale-to-Unknown classification, stays in the reducer owner.
    fn observe_close_work(
        &mut self,
        observation: spaceterm_core::observations::CloseWorkObservation,
        now: spaceterm_core::observations::MonotonicTick,
    ) -> Result<(), ServerFailure>;
    fn prepare(
        &mut self,
        request: ReductionRequest<'_>,
        cursor: CommitCursor,
    ) -> Result<PreparedReduction, ServerFailure>;
    /// Pairing advances only its identity marks and exact control-release scope.
    fn prepare_pairing_mutation(
        &mut self,
        _binding: Option<OperationBinding>,
        _high_water: IdentityHighWater,
        _clients: &[ClientId],
        _cursor: CommitCursor,
    ) -> Result<PreparedReduction, ServerFailure> {
        Err(crate::failure::failure(
            crate::contracts::ServerFailureClass::RuntimeUnavailable,
        ))
    }
    /// Includes store-only receipts, so the next reduction has the current CAS.
    fn committed(&mut self, receipt: CommitReceipt) -> Result<(), ServerFailure>;
    fn not_applied(&mut self);
}

pub trait LedgerPort: Send {
    fn completion_receipt(
        &self,
        _binding: OperationBinding,
    ) -> Result<Option<CommitReceipt>, ServerFailure> {
        Ok(None)
    }

    fn recover(&mut self, recovered: &RecoveredState) -> Result<(), ServerFailure>;
    /// Interrupt unexecuted starts and reconcile accepted retirement using only
    /// exact trusted old-instance cleanup. Atomic requests remain retriable.
    fn prepare_recovery(
        &mut self,
        recovered: &RecoveredState,
        cleanup: &[SessionCleanupObservation],
        context: TransactionContext,
    ) -> Result<Option<LedgerProjection>, ServerFailure>;
    fn lookup(&self, binding: OperationBinding) -> Result<OperationLookup, ServerFailure>;
    /// Only matching atomic pending work can resume reduction. External progress
    /// and AwaitingIntent never confer a resumed executable-effect permit.
    fn resume_atomic(&mut self, request: &CanonicalOperationRequest)
    -> Result<bool, ServerFailure>;
    fn prepare_admission(
        &mut self,
        request: &CanonicalOperationRequest,
        kind: PendingKind,
        context: TransactionContext,
    ) -> Result<LedgerProjection, ServerFailure>;
    fn prepare_external_progress(
        &mut self,
        binding: OperationBinding,
        progress: PendingExternalProgress,
        context: TransactionContext,
    ) -> Result<LedgerProjection, ServerFailure>;
    fn prepare_completion(
        &mut self,
        binding: OperationBinding,
        outcome: &Outcome,
        outcome_revision: OrganizationRevision,
        context: TransactionContext,
    ) -> Result<LedgerProjection, ServerFailure>;
    /// One staged ledger candidate reserves every new purpose-bound stream,
    /// retires revoked ordinary streams, and completes the original operation.
    /// A quota failure applies none of those changes. Separate prepare calls must
    /// not overwrite one another's staged ledger candidate.
    fn prepare_pairing_completion(
        &mut self,
        binding: OperationBinding,
        streams: &[StreamRecord],
        retired_streams: &[StreamBinding],
        outcome: &Outcome,
        outcome_revision: OrganizationRevision,
        context: TransactionContext,
    ) -> Result<LedgerProjection, ServerFailure>;
    fn prepare_acknowledgement(
        &mut self,
        binding: StreamBinding,
        through: OperationSequence,
        context: TransactionContext,
    ) -> Result<LedgerProjection, ServerFailure>;
    /// Comparison and capacity for purpose-bound pairing admission. No unknown
    /// stream is registered by submitting an operation.
    fn pairing_stream(&self, _binding: StreamBinding) -> Result<StreamRecord, ServerFailure> {
        Err(crate::failure::failure(
            crate::contracts::ServerFailureClass::RuntimeUnavailable,
        ))
    }
    fn check_pairing_capacity(
        &self,
        _streams: &[StreamRecord],
        _context: TransactionContext,
    ) -> Result<(), ServerFailure> {
        Err(crate::failure::failure(
            crate::contracts::ServerFailureClass::RuntimeUnavailable,
        ))
    }
    /// Native issuance and ticket invalidation have no pairing-bound operation.
    fn prepare_pairing_stream_changes(
        &mut self,
        _streams: &[StreamRecord],
        _retired: &[StreamBinding],
        _context: TransactionContext,
    ) -> Result<LedgerProjection, ServerFailure> {
        Err(crate::failure::failure(
            crate::contracts::ServerFailureClass::RuntimeUnavailable,
        ))
    }
    /// Must accept every durable receipt, including non-ledger commits.
    fn committed(&mut self, receipt: CommitReceipt) -> Result<(), ServerFailure>;
    fn not_applied(&mut self);
}

pub struct CorePorts {
    pub reducer: Box<dyn ReducerPort>,
    pub ledger: Box<dyn LedgerPort>,
    pub codec: Arc<dyn LedgerCodec>,
    pub validator: Arc<dyn DurabilityValidator>,
}

impl CorePorts {
    /// Compose the domain owners and the physical store's production validator.
    /// Native startup supplies the public identity and owning account home. This
    /// constructor neither loads credentials nor opens another store authority.
    pub fn production(
        identity: spaceterm_host::organization_store::StoreIdentity,
        home: ServerDirectory,
        clock: Arc<dyn crate::contracts::ServerClock>,
    ) -> Result<Self, ServerFailure> {
        Self::production_adapters(identity, home, clock, None)
    }

    /// Compose owning-server discovery and physical containment for retained
    /// Worktree membership. The native owner selects its trusted Git executable.
    pub fn production_with_git_host(
        identity: spaceterm_host::organization_store::StoreIdentity,
        home: ServerDirectory,
        clock: Arc<dyn crate::contracts::ServerClock>,
        git: Arc<dyn crate::git::host::ServerGitHost>,
    ) -> Result<Self, ServerFailure> {
        Self::production_adapters(identity, home, clock, Some(git))
    }

    fn production_adapters(
        identity: spaceterm_host::organization_store::StoreIdentity,
        home: ServerDirectory,
        clock: Arc<dyn crate::contracts::ServerClock>,
        git: Option<Arc<dyn crate::git::host::ServerGitHost>>,
    ) -> Result<Self, ServerFailure> {
        let validator = Arc::new(
            crate::adapters::store::production_services(identity, home.clone())
                .map_err(crate::failure::store_failure)?,
        );
        Ok(Self {
            reducer: Box::new(OrganizationReducerAdapter {
                home,
                clock,
                git,
                repositories: Vec::new(),
                installed: None,
                snapshot: None,
                staged: None,
            }),
            ledger: Box::new(OperationLedgerAdapter {
                installed: None,
                staged: None,
            }),
            codec: Arc::new(spaceterm_core::operation_ledger::canonical::CanonicalCodec),
            validator,
        })
    }
}

type DomainReducer = spaceterm_core::reducer::OrganizationReducer;
type DomainReduction = spaceterm_core::reducer::PreparedReduction;
type DomainLedger = spaceterm_core::operation_ledger::model::OperationLedger;
type DomainLedgerTransition = spaceterm_core::operation_ledger::model::PreparedLedgerTransition;

/// Retains the real reducer's move-only candidate until the physical receipt.
struct OrganizationReducerAdapter {
    home: ServerDirectory,
    clock: Arc<dyn crate::contracts::ServerClock>,
    git: Option<Arc<dyn crate::git::host::ServerGitHost>>,
    repositories: Vec<RepositoryLocator>,
    installed: Option<DomainReducer>,
    snapshot: Option<Arc<OrganizationSnapshot>>,
    staged: Option<(DomainReduction, Arc<OrganizationSnapshot>)>,
}
impl OrganizationReducerAdapter {
    fn reducer(&self) -> Result<&DomainReducer, ServerFailure> {
        self.installed.as_ref().ok_or_else(recovery_failure)
    }
    fn context(
        &self,
        binding: OperationBinding,
    ) -> Result<spaceterm_core::reducer::ReductionContext, ServerFailure> {
        use spaceterm_core::reducer::*;
        Ok(ReductionContext {
            principal: MutationPrincipal {
                pairing: binding.id.stream().pairing(),
                client: binding.id.stream().client(),
            },
            operation: binding.id,
            request_digest: binding.digest,
            now: self.clock.now()?,
            facts: ReductionFacts {
                directories: None,
                retirement_outcome: Some(
                    AppliedRetirementOutcome::new(
                        &spaceterm_core::operation_ledger::canonical::CanonicalCodec,
                    )
                    .map_err(crate::failure::ledger_failure)?,
                ),
            },
        })
    }
    fn stage(&mut self, candidate: DomainReduction) -> Result<PreparedReduction, ServerFailure> {
        use spaceterm_core::reducer::Resolution;
        if self.staged.is_some() {
            return Err(recovery_failure());
        }
        let writes = candidate.writes();
        let mut projection = PreparedReduction {
            snapshot: Arc::new(candidate.snapshot().clone()),
            delta: self
                .reducer()?
                .preview_publication(&candidate)
                .map_err(reducer_failure)?
                .map(Arc::new),
            high_water: writes.resulting_high_water(),
            organization_writes: writes.organization_writes().to_vec(),
            // The actor merges the one operation basis with its final ledger
            // projection. Never retain two independently produced basis writes.
            auxiliary_writes: writes
                .operation_writes()
                .iter()
                .filter(|write| !matches!(write, OperationWrite::ReductionBasis(_)))
                .cloned()
                .collect(),
            outcome: Outcome::Applied,
            retirement: None,
            native_retirement: None,
        };
        match candidate.resolution() {
            Resolution::Completed(outcome) => projection.outcome = outcome.clone(),
            Resolution::AwaitingRetirement(plan) => {
                projection.retirement = Some(plan.descriptor.clone())
            }
            Resolution::AwaitingNativeRetirement(descriptor) => {
                projection.native_retirement = Some(descriptor.as_ref().clone())
            }
            Resolution::RetirementCompleted(plan) | Resolution::GitRetirementReleased(plan) => {
                projection.outcome = plan
                    .descriptor
                    .parts()
                    .eventual_result
                    .outcome(&spaceterm_core::operation_ledger::canonical::CanonicalCodec)
                    .map_err(crate::failure::ledger_failure)?;
            }
            Resolution::NativeRetirementCompleted(_)
            | Resolution::AllocationReserved(_)
            | Resolution::AllocationAbandoned(_) => {}
            Resolution::RetirementProgress { .. } => return Err(recovery_failure()),
        }
        self.staged = Some((candidate, projection.snapshot.clone()));
        Ok(projection)
    }
}
impl ReducerPort for OrganizationReducerAdapter {
    fn prepare_git_cleanup(
        &mut self,
        cleanup: &[SessionCleanupObservation],
        cursor: CommitCursor,
    ) -> Result<Option<PreparedReduction>, ServerFailure> {
        if self.staged.is_some() || self.reducer()?.store_revision() != cursor.store {
            return Err(recovery_failure());
        }
        let candidate = self
            .reducer()?
            .prepare_completed_retirements(&[], cleanup)
            .map_err(reducer_failure)?;
        self.stage(candidate).map(Some)
    }
    fn prepare_git_cancellation(
        &mut self,
        binding: OperationBinding,
        failure: spaceterm_core::worktrees::GitFailure,
        reserved: bool,
        cursor: CommitCursor,
    ) -> Result<PreparedReduction, ServerFailure> {
        use spaceterm_core::worktrees::{
            ExternalDisposition, GitMutationOutcome, GitMutationOutcomeParts,
        };
        if self.staged.is_some() || self.reducer()?.store_revision() != cursor.store {
            return Err(recovery_failure());
        }
        let outcome = Outcome::Git(
            GitMutationOutcome::new(GitMutationOutcomeParts::Failed {
                server: cursor.server,
                failure,
                ended_sessions: 0,
                external: ExternalDisposition::NoEffectEstablished,
            })
            .map_err(|_| recovery_failure())?,
        );
        let candidate = if reserved {
            self.reducer()?.prepare_abandon_git_allocation(binding)
        } else {
            self.reducer()?
                .prepare_outcome(Some(binding), outcome.clone())
        }
        .map_err(reducer_failure)?;
        let mut projection = self.stage(candidate)?;
        projection.outcome = outcome;
        Ok(projection)
    }
    fn git_host(&self) -> Option<Arc<dyn crate::git::host::ServerGitHost>> {
        self.git.clone()
    }

    fn prepare_git_inventory(
        &mut self,
        request: &crate::git::authority::GitInventoryReconciliation,
        cursor: CommitCursor,
    ) -> Result<PreparedReduction, ServerFailure> {
        use spaceterm_core::reducer::{GitInventoryPatch, GitInventoryWorktree};
        if self.staged.is_some() || self.reducer()?.store_revision() != cursor.store {
            return Err(recovery_failure());
        }
        let inventory = &request.inventory;
        let version = request
            .basis
            .organization
            .state()
            .retained
            .as_ref()
            .ok_or_else(recovery_failure)?
            .repository_inventory_versions()
            .iter()
            .find(|v| Some(v.id()) == inventory.repository)
            .map(|v| v.version());
        let mut worktrees: Vec<_> = inventory
            .records
            .iter()
            .map(|v| GitInventoryWorktree {
                id: v.retained,
                directory: v.directory.clone(),
                main: v.main,
                missing: v.missing,
                listed: true,
            })
            .collect();
        // A successful list may omit an administratively removed Worktree. Keep
        // its identity and every reference, marking it unlisted and missing.
        for old in request
            .basis
            .organization
            .state()
            .worktrees
            .iter()
            .filter(|v| Some(v.repository) == inventory.repository)
        {
            if !inventory.records.iter().any(|v| v.retained == Some(old.id)) {
                worktrees.push(GitInventoryWorktree {
                    id: Some(old.id),
                    directory: old.directory.clone(),
                    main: old.main,
                    missing: true,
                    listed: false,
                });
            }
        }
        let candidate = self
            .reducer()?
            .prepare_server_git_inventory(
                &GitInventoryPatch {
                    repository: inventory.repository,
                    directory: inventory.main_root.clone(),
                    expected_inventory_version: version,
                    worktrees,
                },
                &request.memberships,
            )
            .map_err(reducer_failure)?;
        let mut projection = self.stage(candidate)?;
        if projection.outcome != Outcome::Applied {
            self.staged = None;
            return Err(crate::failure::failure(
                crate::contracts::ServerFailureClass::StaleAuthority,
            ));
        }
        let repository = projection
            .snapshot
            .state()
            .repositories
            .iter()
            .find(|v| v.directory == inventory.main_root)
            .ok_or_else(recovery_failure)?
            .id;
        let locator = RepositoryLocator::new(RepositoryLocatorParts {
            scope: RepositoryScope {
                server: cursor.server,
                repository,
            },
            main_root: inventory.main_root.clone(),
            common_directory: inventory.common_directory.clone(),
        })
        .map_err(|_| recovery_failure())?;
        projection
            .auxiliary_writes
            .push(OperationWrite::GitRepository(GitRepositoryMutation::Put(
                locator,
            )));
        Ok(projection)
    }
    fn prepare_git_outside_source(
        &mut self,
        request: &crate::git::authority::GitOutsideSourceReconciliation,
        cursor: CommitCursor,
    ) -> Result<PreparedReduction, ServerFailure> {
        if self.staged.is_some() || self.reducer()?.store_revision() != cursor.store {
            return Err(recovery_failure());
        }
        let state = request.basis.organization.state();
        let repository = request
            .memberships
            .iter()
            .find_map(|edit| {
                state
                    .tabs
                    .iter()
                    .find(|v| v.id == edit.tab)
                    .and_then(|v| v.worktree)
                    .and_then(|id| state.worktrees.iter().find(|v| v.id == id))
                    .map(|v| v.repository)
            })
            .ok_or_else(recovery_failure)?;
        let candidate = self
            .reducer()?
            .prepare_server_worktree_memberships(
                RepositoryScope {
                    server: cursor.server,
                    repository,
                },
                &request.memberships,
            )
            .map_err(reducer_failure)?;
        self.stage(candidate)
    }

    fn prepare_git_retirement(
        &mut self,
        intent: &GitWriteIntent,
        cursor: CommitCursor,
    ) -> Result<PreparedReduction, ServerFailure> {
        if self.staged.is_some() || self.reducer()?.store_revision() != cursor.store {
            return Err(recovery_failure());
        }
        let candidate = self
            .reducer()?
            .prepare_started_git_retirement(intent, &self.context(intent.binding())?)
            .map_err(reducer_failure)?;
        self.stage(candidate)
    }
    fn git_home(&self) -> Option<ServerDirectory> {
        Some(self.home.clone())
    }
    fn prepare_directories(
        &self,
        request: &CanonicalOperationRequest,
    ) -> Result<
        Option<
            crate::contracts::ServerFuture<
                'static,
                Result<spaceterm_core::reducer::DirectoryValidation, ServerFailure>,
            >,
        >,
        ServerFailure,
    > {
        if !matches!(
            request.command(),
            Command::CreateWorkspace { .. }
                | Command::CreateTab { .. }
                | Command::CreatePane { .. }
                | Command::SetPinnedDirectory { .. }
                | Command::RepairPaneDirectory { .. }
        ) {
            return Ok(None);
        }
        let basis = self
            .reducer()?
            .directory_basis(request.command())
            .map_err(reducer_failure)?;
        let home = self.home.clone();
        let binding = request.binding();
        let git = self.git.clone();
        let authority = crate::git::authority::GitAuthoritySnapshot {
            organization: self.reducer()?.snapshot(),
            repositories: self.repositories.clone(),
            home: self.home.clone(),
            display_directories: Vec::new(),
            runtime: spaceterm_core::worktrees::assessment::RuntimeWorkSet { panes: Vec::new() },
        };
        Ok(Some(Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                use spaceterm_core::{
                    reducer::{DirectoryFact, DirectoryValidation},
                    server_directories::{DirectoryPurpose, DirectoryValidationClass},
                };
                use std::{ffi::OsString, os::unix::ffi::OsStringExt, path::PathBuf};
                let provider =
                    spaceterm_host::server_directories::native::NativeDirectoryProvider::new(
                        home.server(),
                        PathBuf::from(OsString::from_vec(home.bytes().to_vec())),
                    )
                    .map_err(|_| {
                        crate::failure::failure(
                            crate::contracts::ServerFailureClass::NativeUnavailable,
                        )
                    })?;
                let worktree = basis.worktree.as_ref().map(|value| &value.worktree);
                if let Some(worktree) = worktree {
                    let git = git.as_ref().ok_or_else(|| crate::failure::failure(crate::contracts::ServerFailureClass::NativeUnavailable))?;
                    let cancellation = crate::git::host::GitReadCancellation::default();
                    let repository = authority.organization.state().repositories.iter().find(|value| value.id == worktree.repository).ok_or_else(recovery_failure)?;
                    let root = git.discover_source(&worktree.directory, &authority, &cancellation).map_err(|_| crate::failure::failure(crate::contracts::ServerFailureClass::NativeUnavailable))?;
                    let repository = git.discover_source(&repository.directory, &authority, &cancellation).map_err(|_| crate::failure::failure(crate::contracts::ServerFailureClass::NativeUnavailable))?;
                    if !matches!((root, repository), (Some(root), Some(repository)) if root.worktree == worktree.directory && root.common_directory == repository.common_directory) {
                        return Err(crate::failure::failure(crate::contracts::ServerFailureClass::NativeUnavailable));
                    }
                }
                let inspect = |directory: ServerDirectory| -> Result<DirectoryFact, ServerFailure> {
                    let observation = provider
                        .validate(&directory, DirectoryPurpose::SessionLaunch)
                        .map_err(|_| {
                            crate::failure::failure(
                                crate::contracts::ServerFailureClass::NativeUnavailable,
                            )
                        })?;
                    let available = observation.class == DirectoryValidationClass::AvailableDirectory;
                    let physically_inside = if let Some(worktree) = worktree.filter(|_| available) {
                        match git.as_ref().ok_or_else(recovery_failure)?.containment(&directory, &worktree.directory) {
                            spaceterm_core::server_directories::DirectoryContainment::Inside => Some(worktree.id),
                            spaceterm_core::server_directories::DirectoryContainment::Outside => None,
                            spaceterm_core::server_directories::DirectoryContainment::Unchecked => return Err(crate::failure::failure(crate::contracts::ServerFailureClass::NativeUnavailable)),
                        }
                    } else { None };
                    Ok(DirectoryFact { directory, available, physically_inside })
                };
                Ok(DirectoryValidation {
                    binding,
                    home: inspect(home)?,
                    pin: basis
                        .pin
                        .as_ref()
                        .and_then(|v| v.value.clone())
                        .map(&inspect)
                        .transpose()?,
                    source: basis
                        .source
                        .as_ref()
                        .map(|v| inspect(v.directory.clone()))
                        .transpose()?,
                    requested: basis.requested.clone().map(&inspect).transpose()?,
                    worktree: worktree.map(|value| inspect(value.directory.clone())).transpose()?,
                    // discover_source above accepts only an actual non-bare working tree.
                    worktree_bare: false,
                    basis,
                })
            })
            .await
            .map_err(|_| {
                crate::failure::failure(crate::contracts::ServerFailureClass::NativeUnavailable)
            })?
        })))
    }

    fn recover(
        &mut self,
        recovered: &RecoveredState,
        epoch: ServerEpoch,
        cleanup: &[SessionCleanupObservation],
    ) -> Result<PreparedReduction, ServerFailure> {
        use spaceterm_core::reducer::{OrganizationLimits, RetainedOrganization};
        let ledger = recovered.ledger.as_ref().ok_or_else(recovery_failure)?;
        let mut retirements = Vec::new();
        for entry in ledger.operations() {
            if let RecoveredOperationRecord::Pending(record) = entry {
                match record.external_progress.as_ref().map(|v| v.state()) {
                    Some(ExternalProgressState::AcceptedRetirement(v))
                    | Some(ExternalProgressState::RecordedIntent {
                        retirement: Some(v),
                        ..
                    }) => retirements.push(v.as_ref().clone()),
                    _ => {}
                }
            }
        }
        let batches: Vec<_> = retirements
            .iter()
            .filter(|v| v.completion() != RetirementCompletion::ContinueGitSettlement)
            .map(|v| v.batch())
            .chain(recovered.native_retirements.iter().map(|v| v.batch()))
            .collect();
        let retained = RetainedOrganization::new(
            recovered.organization.clone(),
            recovered.high_water,
            self.home.clone(),
        )
        .with_pending_retirements(retirements)
        .with_native_retirements(recovered.native_retirements.clone())
        .with_git_allocations(recovered.git_allocations.clone());
        self.installed = Some(
            DomainReducer::from_recovered(
                retained,
                recovered.store_revision,
                epoch,
                OrganizationLimits::default(),
            )
            .map_err(reducer_failure)?,
        );
        self.staged = None;
        self.repositories = recovered.git_repositories.clone();
        self.snapshot = Some(Arc::new(self.reducer()?.snapshot()));
        let candidate = self
            .reducer()?
            .prepare_recovered_retirements(&batches, cleanup)
            .map_err(reducer_failure)?;
        self.stage(candidate)
    }
    fn snapshot(&self) -> Arc<OrganizationSnapshot> {
        // Actor construction calls this only after successful recovery.
        match &self.snapshot {
            Some(snapshot) => snapshot.clone(),
            None => unreachable!("reducer snapshot before validated recovery"),
        }
    }
    fn observe_close_work(
        &mut self,
        observation: spaceterm_core::observations::CloseWorkObservation,
        now: spaceterm_core::observations::MonotonicTick,
    ) -> Result<(), ServerFailure> {
        if self.staged.is_some() {
            return Err(recovery_failure());
        }
        self.installed
            .as_mut()
            .ok_or_else(recovery_failure)?
            .observe_close_work(observation, now)
            .map_err(reducer_failure)
    }
    fn prepare(
        &mut self,
        request: ReductionRequest<'_>,
        cursor: CommitCursor,
    ) -> Result<PreparedReduction, ServerFailure> {
        use spaceterm_core::reducer::{GitAllocationRequest, OrganizationEvent};
        if self.staged.is_some()
            || self.reducer()?.store_revision() != cursor.store
            || self.reducer()?.snapshot().state().revision != cursor.organization
        {
            return Err(recovery_failure());
        }
        let candidate = match request {
            ReductionRequest::Command(request) => match request.command() {
                Command::BeginStartup(intent) => self.reducer()?.prepare_outcome(
                    Some(request.binding()),
                    Outcome::StartupAwaitingMeasurement {
                        intent: intent.id(),
                    },
                ),
                Command::CancelStartup { .. } => self
                    .reducer()?
                    .prepare_outcome(Some(request.binding()), Outcome::Applied),
                _ => self
                    .reducer()?
                    .prepare_command(request.command(), &self.context(request.binding())?),
            },
            ReductionRequest::ValidatedCommand {
                request,
                directories,
            } => {
                let mut context = self.context(request.binding())?;
                context.facts.directories = Some(directories.clone());
                self.reducer()?.prepare_command(request.command(), &context)
            }
            ReductionRequest::Rejected { binding, failure } => self
                .reducer()?
                .prepare_outcome(Some(binding), Outcome::Failed(failure)),
            ReductionRequest::Control {
                binding,
                transition,
            } => {
                let expected = transition.expected();
                let next = transition.next();
                let current = self.reducer()?.snapshot();
                if transition.epoch() != current.epoch()
                    || current.runtime().iter().find(|v| v.pane == expected.pane) != Some(&expected)
                {
                    return Err(crate::failure::failure(
                        crate::contracts::ServerFailureClass::StaleAuthority,
                    ));
                }
                let outcome = transition
                    .granted()
                    .map_or(Outcome::Applied, Outcome::ControlGranted);
                self.reducer()?.prepare_events(
                    &[OrganizationEvent::ControlChanged {
                        pane: next.pane,
                        expected: expected.control,
                        replacement: next.control,
                    }],
                    Some(binding),
                    outcome,
                )
            }
            ReductionRequest::StartupPreparationFailed {
                binding,
                intent,
                failure,
            } => {
                let event = OrganizationEvent::DirectoryUnavailable {
                    pane: intent.fence().session().pane,
                    expected_generation: intent.fence().session().generation,
                };
                if failure == OperationFailure::DirectoryUnavailable {
                    self.reducer()?.prepare_events(
                        &[event],
                        Some(binding),
                        Outcome::Failed(failure),
                    )
                } else {
                    self.reducer()?
                        .prepare_outcome(Some(binding), Outcome::Failed(failure))
                }
            }
            ReductionRequest::StartupReserved {
                binding,
                reservation,
            } => {
                let runtime = self.reducer()?.snapshot();
                let old = runtime
                    .runtime()
                    .iter()
                    .find(|v| v.pane == reservation.session().pane)
                    .ok_or_else(recovery_failure)?;
                let startup = StartupReservation::new(
                    old.pane,
                    old.session_generation,
                    old.control_generation,
                    old.control,
                    reservation.session(),
                )
                .map_err(|_| recovery_failure())?;
                self.reducer()?.prepare_events(
                    &[OrganizationEvent::StartupReserved(startup)],
                    Some(binding),
                    Outcome::SessionStarted(reservation.session()),
                )
            }
            ReductionRequest::RuntimeStarted(session) => self
                .reducer()?
                .prepare_event(&OrganizationEvent::RuntimeStarted { session }),
            ReductionRequest::RuntimeFinished(terminal) => {
                self.reducer()?.prepare_terminal_result(terminal)
            }
            ReductionRequest::RuntimeCleanup(observation) => {
                self.reducer()?.prepare_runtime_cleanup(observation)
            }
            ReductionRequest::UnstartedLaunchFailed { terminal, cleanup } => {
                self.reducer()?.prepare_unstarted_failure(terminal, cleanup)
            }
            ReductionRequest::RestorationDirectoryConfirmed(observation) => self
                .reducer()?
                .prepare_event(&OrganizationEvent::RestorationDirectoryConfirmed(
                    observation.clone(),
                )),
            ReductionRequest::RetirementCompleted {
                descriptor,
                cleanup,
            } => self
                .reducer()?
                .prepare_completed_retirements(&[descriptor.batch()], cleanup),
            ReductionRequest::NativeRetirementAccepted {
                request, action, ..
            } => self
                .reducer()?
                .prepare_native_retirement(request, action, self.clock.now()?),
            ReductionRequest::NativeRetirementObserved { cleanup, .. } => {
                self.reducer()?.prepare_completed_retirements(&[], cleanup)
            }
            ReductionRequest::NativeRetirementCompleted {
                descriptor,
                cleanup,
            } => self
                .reducer()?
                .prepare_completed_retirements(&[descriptor.batch()], cleanup),
            ReductionRequest::ReleaseClients(clients) => {
                self.reducer()?
                    .prepare_principal_change(None, cursor.high_water, clients)
            }
            ReductionRequest::PairingCompletion {
                binding,
                allocation,
                revoked_clients,
            } => {
                let mut marks = cursor.high_water;
                if let crate::extensions::PairingAllocationRequest::AdditionalClient(_) = allocation
                {
                    marks.client = marks.client.checked_add(1).ok_or_else(recovery_failure)?;
                    marks.operation_stream = marks
                        .operation_stream
                        .checked_add(2)
                        .ok_or_else(recovery_failure)?;
                }
                self.reducer()?
                    .prepare_principal_change(Some(binding), marks, revoked_clients)
            }
            ReductionRequest::ReserveGitAllocation {
                binding,
                repository,
                worktree,
                fallbacks,
            } => self.reducer()?.prepare_git_allocation(
                &GitAllocationRequest {
                    repository,
                    worktree,
                    fallbacks: fallbacks.to_vec(),
                },
                &self.context(binding)?,
            ),
            ReductionRequest::StartedGitRetirement { intent, descriptor } => {
                let candidate = self.reducer()?.prepare_started_git_retirement(intent, &self.context(intent.binding())?)
                    .map_err(reducer_failure)?;
                if !matches!(candidate.resolution(), spaceterm_core::reducer::Resolution::AwaitingRetirement(plan)
                    if &plan.descriptor == descriptor)
                {
                    return Err(crate::failure::failure(crate::contracts::ServerFailureClass::StaleAuthority));
                }
                Ok(candidate)
            }
            ReductionRequest::SettleStartedGit { intent, result, revoked_clients } => {
                self.reducer()?.prepare_git_settlement(intent, result,
                    !revoked_clients.contains(&intent.binding().id.stream().client()))
            }
        }
        .map_err(reducer_failure)?;
        let settlement = match request {
            ReductionRequest::SettleStartedGit { intent, result, .. } => Some((intent, result)),
            _ => None,
        };
        let mut projection = self.stage(candidate)?;
        if let Some((intent, result)) = settlement {
            use spaceterm_core::worktrees::{
                ExternalDisposition, GitFailure, GitMutationOutcome, GitMutationOutcomeParts,
            };
            // A conflicting destructive reservation is retried, never acknowledged
            // as successful external settlement or allowed to resurrect hierarchy.
            if let Outcome::Failed(failure) = projection.outcome {
                self.staged = None;
                return Err(crate::failure::failure(match failure {
                    OperationFailure::BusyRetiring => {
                        crate::contracts::ServerFailureClass::Capacity
                    }
                    _ => crate::contracts::ServerFailureClass::StaleAuthority,
                }));
            }
            let revision = projection.snapshot.state().revision;
            let success = result.disposition == ExternalDisposition::DesiredStateObserved
                && result.failure.is_none();
            let outcome = if success {
                match intent.parts().action {
                    RetainedGitAction::Create { worktree, .. } => {
                        GitMutationOutcomeParts::Created { worktree, revision }
                    }
                    RetainedGitAction::Remove { worktree, .. } => {
                        GitMutationOutcomeParts::Removed {
                            worktree,
                            closed_tabs: intent.parts().affected.parts().tabs.len() as u32,
                            ended_sessions: result.ended_sessions,
                            revision,
                        }
                    }
                }
            } else {
                GitMutationOutcomeParts::Failed {
                    server: intent.binding().id.stream().server(),
                    failure: result.failure.unwrap_or(GitFailure::ExternalStateUncertain),
                    ended_sessions: result.ended_sessions,
                    external: result.disposition,
                }
            };
            projection.outcome =
                Outcome::Git(GitMutationOutcome::new(outcome).map_err(|_| recovery_failure())?);
            projection.auxiliary_writes.push(OperationWrite::GitIntent(
                GitIntentMutation::Complete {
                    binding: intent.binding(),
                },
            ));
        }
        Ok(projection)
    }
    fn prepare_pairing_mutation(
        &mut self,
        binding: Option<OperationBinding>,
        high_water: IdentityHighWater,
        clients: &[ClientId],
        cursor: CommitCursor,
    ) -> Result<PreparedReduction, ServerFailure> {
        if !high_water.dominates(cursor.high_water) {
            return Err(recovery_failure());
        }
        let candidate = self
            .reducer()?
            .prepare_principal_change(binding, high_water, clients)
            .map_err(reducer_failure)?;
        self.stage(candidate)
    }
    fn committed(&mut self, receipt: CommitReceipt) -> Result<(), ServerFailure> {
        let reducer = self.installed.as_mut().ok_or_else(recovery_failure)?;
        if let Some((candidate, snapshot)) = self.staged.take() {
            reducer
                .apply_committed(candidate, &receipt)
                .map_err(reducer_failure)?;
            self.snapshot = Some(snapshot);
        } else {
            reducer
                .observe_ledger_commit(&receipt)
                .map_err(reducer_failure)?;
        }
        Ok(())
    }
    fn not_applied(&mut self) {
        self.staged = None;
    }
}

struct OperationLedgerAdapter {
    installed: Option<DomainLedger>,
    staged: Option<DomainLedgerTransition>,
}
impl OperationLedgerAdapter {
    fn ledger(&self) -> Result<&DomainLedger, ServerFailure> {
        self.installed.as_ref().ok_or_else(recovery_failure)
    }
    fn ledger_mut(&mut self) -> Result<&mut DomainLedger, ServerFailure> {
        self.installed.as_mut().ok_or_else(recovery_failure)
    }
    fn stage(
        &mut self,
        transition: DomainLedgerTransition,
    ) -> Result<LedgerProjection, ServerFailure> {
        if self.staged.is_some() {
            return Err(recovery_failure());
        }
        let projection = transition.projection().clone();
        self.staged = Some(transition);
        Ok(projection)
    }
}
impl LedgerPort for OperationLedgerAdapter {
    fn completion_receipt(
        &self,
        binding: OperationBinding,
    ) -> Result<Option<CommitReceipt>, ServerFailure> {
        match self
            .ledger()?
            .operation(&StreamBinding::from_stream(binding.id.stream()), binding.id)
            .map_err(crate::failure::ledger_failure)?
        {
            Some(RecoveredOperationRecord::Completed(record)) if record.binding == binding => {
                Ok(Some(CommitReceipt {
                    store_revision: record.completed_at,
                    organization_revision: record.outcome_revision,
                }))
            }
            _ => Ok(None),
        }
    }

    fn recover(&mut self, recovered: &RecoveredState) -> Result<(), ServerFailure> {
        self.installed = Some(
            DomainLedger::recover(
                recovered.organization.server,
                recovered.ledger.clone().ok_or_else(recovery_failure)?,
            )
            .map_err(crate::failure::ledger_failure)?,
        );
        self.staged = None;
        Ok(())
    }
    fn prepare_recovery(
        &mut self,
        recovered: &RecoveredState,
        _: &[SessionCleanupObservation],
        _: TransactionContext,
    ) -> Result<Option<LedgerProjection>, ServerFailure> {
        // Recovery settles external admission and accepted cleanup through their
        // real progress/completion transitions before preparing the new epoch.
        if recovered.ledger.as_ref().ok_or_else(recovery_failure)?.operations().iter().any(|entry| {
            matches!(entry, RecoveredOperationRecord::Pending(record) if matches!(record.external_progress.as_ref().map(|v| v.state()), Some(ExternalProgressState::AcceptedRetirement(_) | ExternalProgressState::AwaitingIntent)))
        }) { return Err(recovery_failure()); }
        Ok(None)
    }
    fn lookup(&self, binding: OperationBinding) -> Result<OperationLookup, ServerFailure> {
        self.ledger()?
            .lookup(
                &StreamBinding::from_stream(binding.id.stream()),
                binding.id,
                Some(binding.digest),
            )
            .map_err(crate::failure::ledger_failure)
    }
    fn resume_atomic(
        &mut self,
        request: &CanonicalOperationRequest,
    ) -> Result<bool, ServerFailure> {
        use spaceterm_core::operation_ledger_contracts::LedgerFailureClass;
        match self.ledger_mut()?.resume_recovered_atomic(
            &StreamBinding::from_stream(request.binding().id.stream()),
            request.binding().id,
            request.command(),
        ) {
            Ok(_) => Ok(true),
            Err(error) if error.class == LedgerFailureClass::InvalidTransition => Ok(false),
            Err(error) => Err(crate::failure::ledger_failure(error)),
        }
    }
    fn prepare_admission(
        &mut self,
        request: &CanonicalOperationRequest,
        kind: PendingKind,
        context: TransactionContext,
    ) -> Result<LedgerProjection, ServerFailure> {
        use spaceterm_core::operation_ledger::model::AdmissionPreparation;
        match self
            .ledger_mut()?
            .prepare_admission(
                &StreamBinding::from_stream(request.binding().id.stream()),
                request.binding().id,
                request.command(),
                kind,
                context,
            )
            .map_err(crate::failure::ledger_failure)?
        {
            AdmissionPreparation::Prepared(transition) => self.stage(transition),
            AdmissionPreparation::Existing(_) => Err(recovery_failure()),
        }
    }
    fn prepare_external_progress(
        &mut self,
        binding: OperationBinding,
        progress: PendingExternalProgress,
        context: TransactionContext,
    ) -> Result<LedgerProjection, ServerFailure> {
        let transition = self
            .ledger_mut()?
            .prepare_external_progress(
                &StreamBinding::from_stream(binding.id.stream()),
                binding.id,
                binding.digest,
                progress,
                context,
            )
            .map_err(crate::failure::ledger_failure)?;
        self.stage(transition)
    }
    fn prepare_completion(
        &mut self,
        binding: OperationBinding,
        outcome: &Outcome,
        revision: OrganizationRevision,
        context: TransactionContext,
    ) -> Result<LedgerProjection, ServerFailure> {
        let outcome = CompactOutcome::new(
            outcome,
            &spaceterm_core::operation_ledger::canonical::CanonicalCodec,
        )
        .map_err(crate::failure::ledger_failure)?;
        let transition = self
            .ledger_mut()?
            .prepare_completion(
                &StreamBinding::from_stream(binding.id.stream()),
                binding.id,
                binding.digest,
                outcome,
                revision,
                context,
            )
            .map_err(crate::failure::ledger_failure)?;
        self.stage(transition)
    }
    fn prepare_pairing_completion(
        &mut self,
        binding: OperationBinding,
        streams: &[StreamRecord],
        retired: &[StreamBinding],
        outcome: &Outcome,
        revision: OrganizationRevision,
        context: TransactionContext,
    ) -> Result<LedgerProjection, ServerFailure> {
        let outcome = CompactOutcome::new(
            outcome,
            &spaceterm_core::operation_ledger::canonical::CanonicalCodec,
        )
        .map_err(crate::failure::ledger_failure)?;
        let mut transition = self
            .ledger_mut()?
            .prepare_completion(
                &StreamBinding::from_stream(binding.id.stream()),
                binding.id,
                binding.digest,
                outcome,
                revision,
                context,
            )
            .map_err(crate::failure::ledger_failure)?;
        // Pairing records name each Client's initial streams. Every later ordinary
        // stream for those pairings is retired too, including restricted history.
        let pairings: std::collections::BTreeSet<_> = retired.iter().map(|v| v.pairing()).collect();
        let additions = streams
            .iter()
            .cloned()
            .map(LedgerWrite::RegisterStream)
            .chain(
                self.ledger()?
                    .streams()
                    .filter(|stream| {
                        pairings.contains(&stream.binding.pairing())
                            && stream.status == StreamStatus::Active
                    })
                    .map(|stream| LedgerWrite::RetireStream {
                        binding: stream.binding,
                        reason: StreamRetirementReason::PairingRevoked,
                    }),
            )
            .collect();
        if let Err(error) = self
            .ledger_mut()?
            .extend_prepared(&mut transition, additions)
        {
            self.ledger_mut()?
                .resolve_commit(
                    transition,
                    CommitDisposition::NotApplied(StoreFailure::InvalidTransaction),
                )
                .map_err(crate::failure::ledger_failure)?;
            return Err(crate::failure::ledger_failure(error));
        }
        self.stage(transition)
    }
    fn pairing_stream(&self, binding: StreamBinding) -> Result<StreamRecord, ServerFailure> {
        self.ledger()?
            .stream(&binding)
            .cloned()
            .map_err(crate::failure::ledger_failure)
    }
    fn check_pairing_capacity(
        &self,
        streams: &[StreamRecord],
        context: TransactionContext,
    ) -> Result<(), ServerFailure> {
        if streams.is_empty() {
            return Ok(());
        }
        let mut preview =
            DomainLedger::recover(streams[0].binding.server(), self.ledger()?.retained())
                .map_err(crate::failure::ledger_failure)?;
        pairing_stream_changes(&mut preview, streams, &[], context)?;
        Ok(())
    }
    fn prepare_pairing_stream_changes(
        &mut self,
        streams: &[StreamRecord],
        retired: &[StreamBinding],
        context: TransactionContext,
    ) -> Result<LedgerProjection, ServerFailure> {
        let candidate = pairing_stream_changes(self.ledger_mut()?, streams, retired, context)?;
        self.stage(candidate)
    }
    fn prepare_acknowledgement(
        &mut self,
        binding: StreamBinding,
        through: OperationSequence,
        context: TransactionContext,
    ) -> Result<LedgerProjection, ServerFailure> {
        match self
            .ledger_mut()?
            .prepare_acknowledgement(&binding, through.get(), context)
            .map_err(crate::failure::ledger_failure)?
        {
            Some(transition) => self.stage(transition),
            // Repeated acknowledgements still receive a checked store-only
            // receipt. Registering the identical header changes no authority.
            None => {
                let purpose = self
                    .ledger()?
                    .stream(&binding)
                    .map_err(crate::failure::ledger_failure)?
                    .purpose;
                let transition = self
                    .ledger_mut()?
                    .prepare_register_stream_with_purpose(binding, purpose, context)
                    .map_err(crate::failure::ledger_failure)?;
                self.stage(transition)
            }
        }
    }
    fn committed(&mut self, receipt: CommitReceipt) -> Result<(), ServerFailure> {
        if let Some(transition) = self.staged.take() {
            self.ledger_mut()?
                .resolve_commit(transition, CommitDisposition::Durable(receipt))
                .map_err(crate::failure::ledger_failure)?;
        } else {
            self.ledger_mut()?
                .observe_store_commit(receipt)
                .map_err(crate::failure::ledger_failure)?;
        }
        Ok(())
    }
    fn not_applied(&mut self) {
        if let Some(transition) = self.staged.take()
            && let Some(ledger) = &mut self.installed
        {
            let _ = ledger.resolve_commit(
                transition,
                CommitDisposition::NotApplied(StoreFailure::InvalidTransaction),
            );
        }
    }
}
fn pairing_stream_changes(
    ledger: &mut DomainLedger,
    streams: &[StreamRecord],
    retired: &[StreamBinding],
    context: TransactionContext,
) -> Result<DomainLedgerTransition, ServerFailure> {
    let mut candidate = if let Some(first) = streams.first() {
        ledger.prepare_register_stream_with_purpose(first.binding, first.purpose, context)
    } else {
        ledger.prepare_retirement(retired, StreamRetirementReason::Expired, context)
    }
    .map_err(crate::failure::ledger_failure)?;
    let additions = streams
        .iter()
        .skip(1)
        .cloned()
        .map(LedgerWrite::RegisterStream)
        .chain(
            if streams.is_empty() { &[][..] } else { retired }
                .iter()
                .map(|binding| LedgerWrite::RetireStream {
                    binding: *binding,
                    reason: StreamRetirementReason::Expired,
                }),
        )
        .collect();
    if let Err(error) = ledger.extend_prepared(&mut candidate, additions) {
        ledger
            .resolve_commit(
                candidate,
                CommitDisposition::NotApplied(StoreFailure::InvalidTransaction),
            )
            .map_err(crate::failure::ledger_failure)?;
        return Err(crate::failure::ledger_failure(error));
    }
    Ok(candidate)
}
fn recovery_failure() -> ServerFailure {
    crate::failure::failure(crate::contracts::ServerFailureClass::RecoveryRequired)
}
fn reducer_failure(error: spaceterm_core::reducer::ReducerFailure) -> ServerFailure {
    use crate::contracts::ServerFailureClass;
    use spaceterm_core::reducer::ReducerFailure;
    crate::failure::failure(match error {
        ReducerFailure::WrongServer => ServerFailureClass::ScopeMismatch,
        ReducerFailure::TargetNotFound
        | ReducerFailure::WrongMembership
        | ReducerFailure::InvalidFacts
        | ReducerFailure::WrongReducer
        | ReducerFailure::StalePreparation => ServerFailureClass::StaleAuthority,
        ReducerFailure::QuotaExceeded => ServerFailureClass::Capacity,
        ReducerFailure::CounterExhausted => ServerFailureClass::Exhausted,
        _ => ServerFailureClass::RecoveryRequired,
    })
}

/// Merge exact native cleanup into the original descriptor, never a new reason
/// or operation. Missing, duplicate or conflicting evidence fails closed.
pub(crate) fn observed_retirement(
    descriptor: &RetirementDescriptor,
    observations: &[SessionCleanupObservation],
) -> Result<RetirementDescriptor, ServerFailure> {
    let mut parts = descriptor.parts().clone();
    for session in &mut parts.sessions {
        let matching: Vec<_> = observations
            .iter()
            .filter(|v| v.session() == session.session())
            .collect();
        if matching.len() != 1 || matching[0].first_reason() != session.first_reason() {
            return Err(crate::failure::failure(
                crate::contracts::ServerFailureClass::CleanupIncomplete,
            ));
        }
        let state = match matching[0].result() {
            CleanupResult::Confirmed(value) => RetirementCleanupState::Confirmed(value),
            CleanupResult::Failed(value) => RetirementCleanupState::Failed(value),
        };
        if session.cleanup() != RetirementCleanupState::AwaitingEvidence
            && session.cleanup() != state
        {
            return Err(recovery_failure());
        }
        *session = RetiredSession::new(session.session(), session.first_reason(), state)
            .map_err(|_| recovery_failure())?;
    }
    RetirementDescriptor::new(parts).map_err(|_| recovery_failure())
}

pub(crate) fn observed_native_retirement(
    descriptor: &NativeRetirementDescriptor,
    observations: &[SessionCleanupObservation],
) -> Result<NativeRetirementDescriptor, ServerFailure> {
    let mut parts = descriptor.parts().clone();
    for session in &mut parts.sessions {
        let matching: Vec<_> = observations
            .iter()
            .filter(|v| v.session() == session.session())
            .collect();
        if matching.len() != 1 || matching[0].first_reason() != session.first_reason() {
            return Err(crate::failure::failure(
                crate::contracts::ServerFailureClass::CleanupIncomplete,
            ));
        }
        let state = match matching[0].result() {
            CleanupResult::Confirmed(value) => RetirementCleanupState::Confirmed(value),
            CleanupResult::Failed(value) => RetirementCleanupState::Failed(value),
        };
        if session.cleanup() != RetirementCleanupState::AwaitingEvidence
            && session.cleanup() != state
        {
            return Err(recovery_failure());
        }
        *session = RetiredSession::new(session.session(), session.first_reason(), state)
            .map_err(|_| recovery_failure())?;
    }
    NativeRetirementDescriptor::new(parts).map_err(|_| recovery_failure())
}
