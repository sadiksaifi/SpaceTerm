//! Pure shared-organization authority. Preparation supplies no executable authority.
//! The actor admits operations, commits organization and ledger projections together,
//! and applies the exact durable receipt before publishing or executing effects.

mod assessment;
mod close;
pub mod contracts;
mod creation;
mod fields;
mod lifecycle;
mod naming;
mod publication;
mod server_settings;
mod state;
mod topology;

pub use assessment::AssessmentFailure;
pub use contracts::*;
pub use state::{OrganizationLimits, RetainedOrganization};

use crate::authority::*;
use crate::identity::*;
use crate::operations::*;
use crate::organization::*;
use crate::storage::CommitReceipt;
use state::{ReducerState, ReductionPatch};

pub struct OrganizationReducer {
    pub(crate) state: ReducerState,
}

/// A move-only candidate. Writes and result projections are inspectable, but
/// publication and runtime effects become available only after receipt comparison.
pub struct PreparedReduction {
    pub(crate) basis: u64,
    pub(crate) incarnation: std::sync::Arc<()>,
    pub(crate) expected_store: StoreRevision,
    pub(crate) base_revision: OrganizationRevision,
    pub(crate) candidate: ReducerState,
    pub(crate) writes: OrganizationWriteSet,
    pub(crate) effects: Vec<OrganizationEffect>,
    pub(crate) resolution: Resolution,
    pub(crate) accepted_creations: Vec<(TabId, PaneId, CreationOrdinal, PaneCreation)>,
}

pub struct CommittedReduction {
    pub publication: Option<OrganizationDelta>,
    pub effects: Vec<OrganizationEffect>,
    pub resolution: Resolution,
}

impl OrganizationReducer {
    pub fn from_recovered(
        retained: RetainedOrganization,
        store_revision: StoreRevision,
        epoch: ServerEpoch,
        limits: OrganizationLimits,
    ) -> Result<Self, ReducerFailure> {
        Ok(Self {
            state: ReducerState::recover(retained, store_revision, epoch, limits)?,
        })
    }

    pub fn snapshot(&self) -> OrganizationSnapshot {
        self.state.snapshot.clone()
    }

    pub fn close_work_observation(&self, pane: PaneId) -> Option<CloseWorkObservation> {
        self.state
            .observations
            .get(&pane)
            .and_then(|value| value.close_work)
    }

    pub fn confirmed_directory_sequence(&self, pane: PaneId) -> Option<RuntimeObservationSequence> {
        self.state
            .observations
            .get(&pane)
            .and_then(|value| value.confirmed_watermark)
    }

    pub fn retained(&self) -> RetainedOrganization {
        self.state.retained()
    }

    /// Comparison identity for the next staged batch. Job B/C advance the
    /// candidate's sequence only in the accepted reduction. This is not a permit.
    pub fn next_retirement_batch(
        &self,
    ) -> Result<crate::retirement::RetirementBatchId, ReducerFailure> {
        self.state.next_retirement_batch()
    }

    pub fn store_revision(&self) -> StoreRevision {
        self.state.store_revision
    }

    /// Native adapters capture this comparison basis before asynchronous directory
    /// validation. A DirectoryValidation is server-internal data, not a wire claim.
    pub fn directory_basis(&self, command: &Command) -> Result<DirectoryBasis, ReducerFailure> {
        self.state.directory_basis(command)
    }

    pub fn prepare_command(
        &self,
        command: &Command,
        context: &ReductionContext,
    ) -> Result<PreparedReduction, ReducerFailure> {
        self.state.check_admission()?;
        self.state.check_principal(context)?;
        if command.validate_scope(context.operation.stream()).is_err() {
            return Err(ReducerFailure::WrongServer);
        }
        let patch = match plan_organization_command(&self.state, command, context) {
            Ok(patch) => patch,
            Err(ReducerFailure::QuotaExceeded) => {
                ReductionPatch::rejected(&self.state, OperationFailure::Capacity)
            }
            Err(error) => return Err(error),
        };
        self.prepare_patch(patch, Some(context.binding()))
    }

    /// Events are trusted actor inputs, never protocol client requests.
    pub fn prepare_event(
        &self,
        event: &OrganizationEvent,
    ) -> Result<PreparedReduction, ReducerFailure> {
        self.state.check_admission()?;
        match event {
            OrganizationEvent::WorktreeOrganizationChanged(patch) => self.prepare_patch(
                creation::plan_worktree_patch(&self.state, patch)?,
                Some(patch.binding),
            ),
            _ => self.prepare_patch(lifecycle::plan_lifecycle_event(&self.state, event)?, None),
        }
    }

    /// Stage admitted inventory metadata and fresh identities atomically. This
    /// cannot retire Sessions, create fallback Tabs, or authorize an external write.
    pub fn prepare_git_inventory(
        &self,
        inventory: &GitInventoryPatch,
        context: &ReductionContext,
    ) -> Result<PreparedReduction, ReducerFailure> {
        self.prepare_git_inventory_reconciliation(inventory, &[], context)
    }

    /// Reconcile membership against entries whose identities this reducer allocates.
    /// The inventory and membership changes share one candidate and publication revision.
    pub fn prepare_git_inventory_reconciliation(
        &self,
        inventory: &GitInventoryPatch,
        memberships: &[GitInventoryMembership],
        context: &ReductionContext,
    ) -> Result<PreparedReduction, ReducerFailure> {
        self.state.check_admission()?;
        self.state.check_principal(context)?;
        let patch = match creation::plan_git_inventory(&self.state, inventory, memberships, context)
        {
            Ok(patch) => patch,
            Err(ReducerFailure::QuotaExceeded) => {
                ReductionPatch::rejected(&self.state, OperationFailure::Capacity)
            }
            Err(error) => return Err(error),
        };
        self.prepare_patch(patch, Some(context.binding()))
    }

    pub fn prepare_git_allocation(
        &self,
        request: &GitAllocationRequest,
        context: &ReductionContext,
    ) -> Result<PreparedReduction, ReducerFailure> {
        self.state.check_admission()?;
        self.state.check_principal(context)?;
        self.prepare_patch(
            creation::plan_git_allocation(&self.state, request, context)?,
            Some(context.binding()),
        )
    }

    /// Service-owned cancellation releases the reservation, never its identities.
    pub fn prepare_abandon_git_allocation(
        &self,
        binding: OperationBinding,
    ) -> Result<PreparedReduction, ReducerFailure> {
        self.state.check_admission()?;
        let Some(reservation) = self.state.git_allocations.get(&binding.id) else {
            return Err(ReducerFailure::TargetNotFound);
        };
        if reservation.binding() != binding {
            return Err(ReducerFailure::InvalidFacts);
        }
        let mut patch = ReductionPatch::unchanged(&self.state, Outcome::Applied);
        patch.candidate.git_allocations.remove(&binding.id);
        patch
            .operations
            .push(crate::storage::OperationWrite::GitAllocation(
                crate::worktrees::intent::GitAllocationMutation::Abandon { binding },
            ));
        patch.resolution = Resolution::AllocationAbandoned(binding);
        self.prepare_patch(patch, Some(binding))
    }

    /// A native request identity is mandatory. Use prepare_native_retirement after
    /// the native provider durably admits Stop; this legacy signature fails closed.
    pub fn prepare_stop(&self, _now: ReducerTick) -> Result<PreparedReduction, ReducerFailure> {
        self.state.check_admission()?;
        Err(ReducerFailure::InvalidFacts)
    }

    pub fn apply_committed(
        &mut self,
        prepared: PreparedReduction,
        receipt: &CommitReceipt,
    ) -> Result<CommittedReduction, ReducerFailure> {
        self.state.check_admission()?;
        if !std::sync::Arc::ptr_eq(&prepared.incarnation, &self.state.incarnation)
            || prepared.basis != self.state.transition
            || prepared.expected_store != self.state.store_revision
            || prepared.base_revision != self.state.organization().revision
        {
            return Err(ReducerFailure::StalePreparation);
        }
        let next_store = self
            .state
            .store_revision
            .checked_successor()
            .ok_or(ReducerFailure::CounterExhausted)?;
        if receipt.store_revision != next_store
            || receipt.organization_revision != prepared.candidate.organization().revision
        {
            return Err(ReducerFailure::InvalidCommitReceipt);
        }
        let transition = self
            .state
            .transition
            .checked_add(1)
            .ok_or(ReducerFailure::CounterExhausted)?;
        let publication = publication::delta(&self.state, &prepared)?;
        let mut candidate = prepared.candidate;
        candidate.store_revision = receipt.store_revision;
        candidate.transition = transition;
        self.state = candidate;
        Ok(CommittedReduction {
            publication,
            effects: prepared.effects,
            resolution: prepared.resolution,
        })
    }

    pub fn observe_ledger_commit(&mut self, receipt: &CommitReceipt) -> Result<(), ReducerFailure> {
        self.observe_principal_commit(receipt, self.state.high_water)
    }

    /// Pairing and ledger owners allocate principal/stream identities. Observe
    /// their durable marks without changing organization, Roots or field versions.
    /// Organization identity marks can advance only through a reducer preparation.
    pub fn observe_principal_commit(
        &mut self,
        receipt: &CommitReceipt,
        high_water: IdentityHighWater,
    ) -> Result<(), ReducerFailure> {
        self.state.check_admission()?;
        let old = self.state.high_water;
        if !high_water.dominates(old)
            || high_water.workspace != old.workspace
            || high_water.tab != old.tab
            || high_water.pane != old.pane
            || high_water.repository != old.repository
            || high_water.worktree != old.worktree
            || high_water.creation_ordinal != old.creation_ordinal
        {
            return Err(ReducerFailure::InvalidFacts);
        }
        if self.state.store_revision.checked_successor() != Some(receipt.store_revision)
            || receipt.organization_revision != self.state.organization().revision
        {
            return Err(ReducerFailure::InvalidCommitReceipt);
        }
        let next = self
            .state
            .transition
            .checked_add(1)
            .ok_or(ReducerFailure::CounterExhausted)?;
        self.state.store_revision = receipt.store_revision;
        self.state.transition = next;
        self.state.high_water = high_water;
        Ok(())
    }

    /// Inspect a staged candidate without installing or publishing it.
    pub fn preview_publication(
        &self,
        prepared: &PreparedReduction,
    ) -> Result<Option<OrganizationDelta>, ReducerFailure> {
        publication::delta(&self.state, prepared)
    }

    /// Actor-owned store-only completion, including rejected admitted requests.
    pub fn prepare_outcome(
        &self,
        binding: Option<OperationBinding>,
        outcome: Outcome,
    ) -> Result<PreparedReduction, ReducerFailure> {
        self.state.check_admission()?;
        self.prepare_patch(ReductionPatch::unchanged(&self.state, outcome), binding)
    }

    /// Compose trusted events in one candidate and one publication revision.
    /// Intermediate candidates grant no authority and receive no commit receipts.
    pub fn prepare_events(
        &self,
        events: &[OrganizationEvent],
        binding: Option<OperationBinding>,
        outcome: Outcome,
    ) -> Result<PreparedReduction, ReducerFailure> {
        self.state.check_admission()?;
        if events.len() > crate::retirement::MAX_RETIREMENT_PANES {
            return Err(ReducerFailure::QuotaExceeded);
        }
        let mut combined = ReductionPatch::unchanged(&self.state, outcome.clone());
        for event in events {
            let patch = lifecycle::plan_lifecycle_event(&combined.candidate, event)?;
            if matches!(&patch.resolution, Resolution::Completed(Outcome::Failed(_))) {
                return self.prepare_patch(patch, binding);
            }
            combined.candidate = patch.candidate;
            combined.effects.extend(patch.effects);
            combined.operations.extend(patch.operations);
        }
        combined.resolution = Resolution::Completed(outcome);
        self.prepare_patch(combined, binding)
    }

    /// Principal allocation and revocation never allocate organization identities.
    /// Every released grant advances its original control high-water exactly once.
    pub fn prepare_principal_change(
        &self,
        binding: Option<OperationBinding>,
        high_water: IdentityHighWater,
        clients: &[ClientId],
    ) -> Result<PreparedReduction, ReducerFailure> {
        let old = self.state.high_water;
        if !high_water.dominates(old)
            || high_water.workspace != old.workspace
            || high_water.tab != old.tab
            || high_water.pane != old.pane
            || high_water.repository != old.repository
            || high_water.worktree != old.worktree
            || high_water.creation_ordinal != old.creation_ordinal
            || clients
                .iter()
                .any(|client| client.server() != self.state.organization().server)
        {
            return Err(ReducerFailure::InvalidFacts);
        }
        let events = self
            .state
            .snapshot
            .runtime()
            .iter()
            .filter_map(|record| match record.control {
                ControlRecord::Held { client, .. } if clients.contains(&client) => {
                    Some(OrganizationEvent::ControlChanged {
                        pane: record.pane,
                        expected: record.control,
                        replacement: ControlRecord::Unclaimed,
                    })
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        let mut prepared = self.prepare_events(&events, binding, Outcome::Applied)?;
        prepared.candidate.high_water = high_water;
        prepared.writes.resulting_marks = high_water;
        Ok(prepared)
    }

    /// Volatile assessment observations retain the producer's original tick.
    /// They do not install organization metadata or consume a store revision.
    pub fn observe_close_work(
        &mut self,
        observation: CloseWorkObservation,
        now: ReducerTick,
    ) -> Result<(), ReducerFailure> {
        if observation.observed_at() > now {
            return Err(ReducerFailure::InvalidFacts);
        }
        self.state.check_admission()?;
        if self.close_work_observation(observation.session().pane) == Some(observation) {
            return Ok(());
        }
        let patch = lifecycle::plan_lifecycle_event(
            &self.state,
            &OrganizationEvent::CloseWorkObserved(observation),
        )?;
        self.state = patch.candidate;
        self.state.transition = self
            .state
            .transition
            .checked_add(1)
            .ok_or(ReducerFailure::CounterExhausted)?;
        Ok(())
    }

    /// Combine exact trusted cleanup with store-only retirement completion.
    /// A restart, EOF or absent runtime handle is not accepted as evidence.
    pub fn prepare_completed_retirements(
        &self,
        batches: &[crate::retirement::RetirementBatchId],
        cleanup: &[crate::retirement::SessionCleanupObservation],
    ) -> Result<PreparedReduction, ReducerFailure> {
        self.state.check_admission()?;
        if cleanup.len() > crate::retirement::MAX_RETIREMENT_SESSIONS
            || batches.len() > self.state.limits.retirement_batches
        {
            return Err(ReducerFailure::QuotaExceeded);
        }
        let mut combined = ReductionPatch::unchanged(&self.state, Outcome::Applied);
        for observation in cleanup {
            let owner = Self {
                state: combined.candidate.clone(),
            };
            let next = owner.prepare_runtime_cleanup(*observation)?;
            combined.candidate = next.candidate;
            combined
                .operations
                .extend(next.writes.operations.into_iter().filter(|write| {
                    !matches!(
                        write,
                        crate::storage::OperationWrite::NativeRetirement(
                            crate::retirement::NativeRetirementMutation::Progress { .. }
                        )
                    )
                }));
        }
        // Native SQL compares every mutation to the same durable basis. Merge
        // all observations for one request into one progress mutation.
        for (batch, old) in &self.state.native_retirements {
            if let Some(next) = combined.candidate.native_retirements.get(batch)
                && next != old
            {
                // Completion compares already durable cleanup. It cannot share
                // a transaction with this request's first cleanup progress.
                if batches.contains(batch) {
                    return Err(ReducerFailure::InvalidFacts);
                }
                combined
                    .operations
                    .push(crate::storage::OperationWrite::NativeRetirement(
                        crate::retirement::NativeRetirementMutation::Progress {
                            expected: Box::new(old.clone()),
                            next: Box::new(next.clone()),
                        },
                    ));
            }
        }
        for batch in batches {
            let owner = Self {
                state: combined.candidate.clone(),
            };
            let next = owner.prepare_retirement_completion(*batch)?;
            combined.candidate = next.candidate;
            combined.operations.extend(next.writes.operations);
        }
        self.prepare_patch(combined, None)
    }

    /// Resolve a reserved launch whose gate proves process creation never began.
    /// The failed phase and exact absence proof install in one checked receipt.
    pub fn prepare_unstarted_failure(
        &self,
        terminal: crate::runtime_authority::SessionTerminalResult,
        cleanup: crate::retirement::SessionCleanupObservation,
    ) -> Result<PreparedReduction, ReducerFailure> {
        use crate::{
            retirement::{CleanupResult, ConfirmedCleanupClass},
            runtime_authority::RuntimeTerminalStatus,
        };
        if terminal.session() != cleanup.session()
            || !matches!(
                terminal.status(),
                RuntimeTerminalStatus::StartupFailed { .. }
            )
            || cleanup.first_reason() != RetirementReason::RuntimeFailure
            || cleanup.result()
                != CleanupResult::Confirmed(
                    ConfirmedCleanupClass::AlreadyAbsentWithConfirmedOwnership,
                )
            || !self
                .state
                .startup_reservations
                .get(&terminal.session().pane)
                .is_some_and(|reservation| reservation.session() == terminal.session())
        {
            return Err(ReducerFailure::InvalidFacts);
        }
        let finished = self.prepare_terminal_result(terminal)?;
        let owner = Self {
            state: finished.candidate,
        };
        let cleaned = owner.prepare_runtime_cleanup(cleanup)?;
        let mut patch = ReductionPatch::unchanged(&self.state, Outcome::Applied);
        patch.candidate = cleaned.candidate;
        patch.effects = finished.effects;
        patch.effects.extend(cleaned.effects);
        patch.operations = finished.writes.operations;
        patch.operations.extend(cleaned.writes.operations);
        patch.resolution = finished.resolution;
        self.prepare_patch(patch, None)
    }

    /// Settle old-process batches without carrying its Stop barrier into the
    /// newly authorized epoch. Live native completion still stops its own epoch.
    pub fn prepare_recovered_retirements(
        &self,
        batches: &[crate::retirement::RetirementBatchId],
        cleanup: &[crate::retirement::SessionCleanupObservation],
    ) -> Result<PreparedReduction, ReducerFailure> {
        if self.state.stopped
            || batches
                .iter()
                .any(|batch| batch.epoch() >= self.state.epoch())
        {
            return Err(ReducerFailure::InvalidFacts);
        }
        let mut prepared = self.prepare_completed_retirements(batches, cleanup)?;
        prepared.candidate.stopped = false;
        Ok(prepared)
    }

    pub fn suspend_for_recovery(&mut self) {
        self.state.suspended = true;
    }

    pub(crate) fn prepare_patch(
        &self,
        patch: ReductionPatch,
        binding: Option<OperationBinding>,
    ) -> Result<PreparedReduction, ReducerFailure> {
        publication::prepare(&self.state, patch, binding)
    }
}

impl PreparedReduction {
    pub fn snapshot(&self) -> &OrganizationSnapshot {
        &self.candidate.snapshot
    }
    pub fn writes(&self) -> &OrganizationWriteSet {
        &self.writes
    }
    pub fn resolution(&self) -> &Resolution {
        &self.resolution
    }
    pub fn expected_store_revision(&self) -> StoreRevision {
        self.expected_store
    }
    pub fn resulting_organization_revision(&self) -> OrganizationRevision {
        self.candidate.organization().revision
    }
}

pub(crate) fn plan_organization_command(
    state: &ReducerState,
    command: &Command,
    context: &ReductionContext,
) -> Result<ReductionPatch, ReducerFailure> {
    // Stop seals ordinary metadata admission as well as creation/startup. Trusted
    // lifecycle and Started Git settlement use their separate internal ports.
    // Existing deleted-target classifications still precede the barrier.
    if (state.stopped || !state.native_retirements.is_empty())
        && match command {
            Command::RenameWorkspace { workspace, .. }
            | Command::SetPinnedDirectory { workspace, .. } => state
                .organization()
                .workspaces
                .iter()
                .any(|v| v.id == *workspace),
            Command::RepairPaneDirectory { pane, .. } => {
                state.organization().panes.iter().any(|v| v.id == *pane)
            }
            Command::SetWorktreeLocation { .. } | Command::SetRemoteAccess { .. } => true,
            _ => false,
        }
    {
        return Ok(ReductionPatch::rejected(
            state,
            OperationFailure::BusyRetiring,
        ));
    }
    match command {
        Command::CreateWorkspace { .. }
        | Command::CreateTab { .. }
        | Command::CreatePane { .. } => creation::plan(state, command, context),
        Command::RenameWorkspace { .. }
        | Command::SetPinnedDirectory { .. }
        | Command::RepairPaneDirectory { .. } => fields::plan(state, command, context),
        Command::SetWorktreeLocation { .. } | Command::SetRemoteAccess { .. } => {
            server_settings::plan(state, command)
        }
        Command::CloseConfirmed { target, assessment } => close::plan_confirmed_close(
            state,
            &AssessmentTarget::Close(*target),
            assessment,
            context,
        ),
        Command::KillWorkspaceConfirmed {
            workspace,
            assessment,
        } => close::plan_confirmed_close(
            state,
            &AssessmentTarget::KillWorkspace(*workspace),
            assessment,
            context,
        ),
        Command::AssessClose { target } => {
            assessment::plan_assessment(state, &AssessmentTarget::Close(*target), context)
        }
        Command::AssessKillWorkspace { workspace } => assessment::plan_assessment(
            state,
            &AssessmentTarget::KillWorkspace(*workspace),
            context,
        ),
        Command::EndAllSessions { assessment, .. } => {
            lifecycle::plan_end_all(state, assessment, context)
        }
        Command::AssessEndAllSessions { server } => {
            assessment::plan_assessment(state, &AssessmentTarget::EndAllSessions(*server), context)
        }
        _ => Err(ReducerFailure::WrongReducer),
    }
}

pub(crate) use topology::{normalize_creation_default, remove_hierarchy};
