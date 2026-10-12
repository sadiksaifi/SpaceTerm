use super::assessment;
use super::contracts::*;
use super::state::{ReducerState, ReductionPatch};
use crate::operations::*;
use crate::retirement::*;

pub(crate) fn plan_confirmed_close(
    state: &ReducerState,
    intent: &AssessmentTarget,
    handle: &AssessmentRef,
    context: &ReductionContext,
) -> Result<ReductionPatch, ReducerFailure> {
    if !matches!(
        intent,
        AssessmentTarget::Close(_) | AssessmentTarget::KillWorkspace(_)
    ) {
        return Err(ReducerFailure::WrongReducer);
    }
    // Deleted targets remain deleted even if the old handle was consumed or the
    // operation's cleanup is still pending. The ledger owns historical retries.
    if !target_exists(state, *intent) {
        return Ok(ReductionPatch::rejected(
            state,
            OperationFailure::TargetDeleted,
        ));
    }
    let record = match assessment::validate(state, &context.principal, handle, intent, context.now)
    {
        Ok(record) => record,
        Err(failure) => return Ok(ReductionPatch::rejected(state, failure.operation_failure())),
    };
    let panes: Vec<_> = record.panes.iter().map(|v| v.pane).collect();
    let mut retired_panes = Vec::with_capacity(panes.len());
    let mut sessions = Vec::new();
    let mut effects = Vec::new();
    for pane in &record.panes {
        let generation = pane
            .runtime
            .session_generation
            .checked_successor()
            .ok_or(ReducerFailure::CounterExhausted)?;
        let tab = record
            .tabs
            .iter()
            .find(|v| v.id == pane.tab)
            .ok_or(ReducerFailure::InvalidFacts)?;
        retired_panes.push(
            RetirementPane::new(tab.workspace, tab.id, pane.pane, tab.worktree, generation)
                .map_err(|_| ReducerFailure::InvalidFacts)?,
        );
        effects.push(OrganizationEffect::CancelStartupIntents { pane: pane.pane });
        effects.push(OrganizationEffect::InvalidatePaneRuntime {
            pane: pane.pane,
            generation,
            clear_content: true,
            clear_approvals: true,
        });
        if let Some(session) = assessment::live_session(pane, state) {
            if sessions.len() >= MAX_RETIREMENT_SESSIONS {
                return Err(ReducerFailure::QuotaExceeded);
            }
            let reason = match intent {
                AssessmentTarget::KillWorkspace(_) => {
                    crate::organization::RetirementReason::KillWorkspace
                }
                _ => crate::organization::RetirementReason::ConfirmedClose,
            };
            sessions.push(
                RetiredSession::new(session, reason, RetirementCleanupState::AwaitingEvidence)
                    .map_err(|_| ReducerFailure::InvalidFacts)?,
            );
        }
    }
    // Even an empty Session set needs accepted progress before ledger completion.
    let eventual_result = context
        .facts
        .retirement_outcome
        .as_ref()
        .ok_or(ReducerFailure::InvalidFacts)?
        .compact()
        .clone();
    let batch = state.next_retirement_batch()?;
    let descriptor = RetirementDescriptor::new(RetirementDescriptorParts {
        batch,
        binding: context.binding(),
        assessment: *handle,
        acceptance_revision: state
            .organization()
            .revision
            .checked_successor()
            .ok_or(ReducerFailure::CounterExhausted)?,
        target: *intent,
        panes: retired_panes,
        sessions,
        completion: RetirementCompletion::AppliedAtAcceptanceRevision,
        eventual_result,
    })
    .map_err(|error| match error {
        RetirementRecordError::TooLarge => ReducerFailure::QuotaExceeded,
        _ => ReducerFailure::InvalidFacts,
    })?;
    check_retirement_capacity(state, &descriptor)?;
    for session in descriptor.sessions() {
        effects.push(OrganizationEffect::RetireSession {
            batch,
            session: session.session(),
            reason: session.first_reason(),
        });
    }
    // Deletion is the acceptance transaction, never a cleanup continuation. Both
    // ordinary exit and confirmed close call this one cascade and Root policy.
    let mut patch = super::remove_hierarchy(state, &panes)?.into_reduction();
    patch.candidate.assessments.remove(&handle.operation);
    for pane in &panes {
        patch.candidate.startup_reservations.remove(pane);
    }
    patch.effects = effects;
    patch.candidate.retirement_sequence = descriptor.batch().sequence();
    patch
        .candidate
        .retirements
        .insert(descriptor.batch(), descriptor.clone());
    patch.resolution = Resolution::AwaitingRetirement(Box::new(RetirementPlan { descriptor }));
    Ok(patch)
}

fn target_exists(state: &ReducerState, target: AssessmentTarget) -> bool {
    let org = state.organization();
    match target {
        AssessmentTarget::Close(CloseTarget::Pane(id)) => org.panes.iter().any(|v| v.id == id),
        AssessmentTarget::Close(CloseTarget::Tab(id)) => org.tabs.iter().any(|v| v.id == id),
        AssessmentTarget::Close(CloseTarget::Workspace(id))
        | AssessmentTarget::KillWorkspace(id) => org.workspaces.iter().any(|v| v.id == id),
        AssessmentTarget::Close(CloseTarget::WorktreeTabs {
            workspace,
            worktree,
        }) => org
            .tabs
            .iter()
            .any(|v| v.workspace == workspace && v.worktree == Some(worktree)),
        _ => false,
    }
}

/// Preflight aggregate descriptor residency before effects or hierarchy deletion.
/// Native descriptors use the same frozen charge without a compact ledger result.
pub(crate) fn check_retirement_capacity(
    state: &ReducerState,
    descriptor: &RetirementDescriptor,
) -> Result<(), ReducerFailure> {
    if state
        .retirements
        .len()
        .checked_add(state.native_retirements.len())
        .is_none_or(|n| n >= state.limits.retirement_batches)
        || state
            .retirements
            .values()
            .any(|v| v.binding().id == descriptor.binding().id)
    {
        return Err(ReducerFailure::QuotaExceeded);
    }
    let mut bytes = descriptor
        .value_bytes()
        .map_err(|_| ReducerFailure::QuotaExceeded)?;
    for retained in state.retirements.values() {
        bytes = bytes
            .checked_add(
                retained
                    .value_bytes()
                    .map_err(|_| ReducerFailure::QuotaExceeded)?,
            )
            .ok_or(ReducerFailure::QuotaExceeded)?;
    }
    for retained in state.native_retirements.values() {
        let charge = retained
            .panes()
            .len()
            .checked_mul(112)
            .and_then(|n| {
                retained
                    .sessions()
                    .len()
                    .checked_mul(64)
                    .and_then(|s| n.checked_add(s))
            })
            .and_then(|n| n.checked_add(320))
            .ok_or(ReducerFailure::QuotaExceeded)?;
        bytes = bytes
            .checked_add(charge)
            .ok_or(ReducerFailure::QuotaExceeded)?;
    }
    if bytes > state.limits.retirement_bytes {
        return Err(ReducerFailure::QuotaExceeded);
    }
    Ok(())
}
