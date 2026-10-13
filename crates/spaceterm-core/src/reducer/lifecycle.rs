use std::collections::{BTreeMap, BTreeSet};

use super::contracts::*;
use super::state::{ReducerState, ReductionPatch};
use crate::authority::*;
use crate::identity::*;
use crate::operations::*;
use crate::organization::*;
use crate::organization_retained::*;
use crate::retirement::*;

pub(crate) fn plan_lifecycle_event(
    state: &ReducerState,
    event: &OrganizationEvent,
) -> Result<ReductionPatch, ReducerFailure> {
    let mut patch = ReductionPatch::unchanged(state, Outcome::Applied);
    match event {
        OrganizationEvent::StartupReserved(reservation) => {
            let session = reservation.session();
            if session.pane.server() != state.organization().server
                || session.epoch != state.epoch()
            {
                return Err(ReducerFailure::InvalidFacts);
            }
            let Some(mut runtime) = state.runtime(session.pane).ok() else {
                return Ok(patch);
            };
            if state.creation_barrier()
                || pane_pending(state, session.pane)
                || runtime.session_generation != reservation.expected_session_generation()
                || runtime.control_generation != reservation.expected_control_generation()
                || runtime.control != reservation.expected_control()
                || state.startup_reservations.contains_key(&session.pane)
                || matches!(
                    runtime.phase,
                    SessionPhase::Starting { .. }
                        | SessionPhase::Running { .. }
                        | SessionPhase::DirectoryUnavailable
                )
            {
                return Ok(ReductionPatch::rejected(
                    state,
                    OperationFailure::BusyRetiring,
                ));
            }
            runtime.session_generation = session.generation;
            runtime.phase = SessionPhase::Starting {
                generation: session.generation,
            };
            patch
                .candidate
                .startup_reservations
                .insert(session.pane, *reservation);
            patch.candidate.observations.remove(&session.pane);
            replace_runtime(&mut patch.candidate, runtime)?;
        }
        OrganizationEvent::RuntimeStarted { session } => {
            let Some(reservation) = state.startup_reservations.get(&session.pane) else {
                return Ok(patch);
            };
            let mut runtime = state.runtime(session.pane)?;
            if reservation.session() != *session
                || session.epoch != state.epoch()
                || runtime.phase
                    != (SessionPhase::Starting {
                        generation: session.generation,
                    })
                || runtime.control != reservation.expected_control()
                || runtime.control_generation != reservation.expected_control_generation()
                || pane_pending(state, session.pane)
            {
                return Ok(patch);
            }
            runtime.phase = SessionPhase::Running {
                generation: session.generation,
            };
            patch.candidate.startup_reservations.remove(&session.pane);
            replace_runtime(&mut patch.candidate, runtime)?;
        }
        OrganizationEvent::StartupCancelled { session } => {
            if !state
                .startup_reservations
                .get(&session.pane)
                .is_some_and(|v| v.session() == *session)
                || !current_session(state, *session, true)
            {
                return Ok(patch);
            }
            end_runtime(
                &mut patch,
                *session,
                SessionPhase::Ended {
                    reason: EndReason::RuntimeFailure,
                },
            )?;
        }
        OrganizationEvent::RuntimeFailed { session, class } => {
            if !current_session(state, *session, true) {
                return Ok(patch);
            }
            end_runtime(&mut patch, *session, SessionPhase::Failed { class: *class })?;
            let generation = patch.candidate.runtime(session.pane)?.session_generation;
            retain_runtime_cleanup(
                state,
                &mut patch,
                *session,
                generation,
                RetirementReason::RuntimeFailure,
            )?;
        }
        OrganizationEvent::OrdinaryExit { session, .. } => {
            if !current_session(state, *session, true) {
                return Ok(patch);
            }
            let generation = session
                .generation
                .checked_successor()
                .ok_or(ReducerFailure::CounterExhausted)?;
            patch = super::remove_hierarchy(state, &[session.pane])?.into_reduction();
            patch.candidate.startup_reservations.remove(&session.pane);
            patch.effects = invalidation_effects(session.pane, generation);
            retain_runtime_cleanup(
                state,
                &mut patch,
                *session,
                generation,
                RetirementReason::OrdinaryExit,
            )?;
        }
        OrganizationEvent::DirectoryUnavailable {
            pane,
            expected_generation,
        } => {
            let Ok(mut runtime) = state.runtime(*pane) else {
                return Ok(patch);
            };
            if runtime.session_generation != *expected_generation
                || state.creation_barrier()
                || pane_pending(state, *pane)
                || matches!(
                    runtime.phase,
                    SessionPhase::Running { .. }
                        | SessionPhase::Starting { .. }
                        | SessionPhase::DirectoryUnavailable
                )
            {
                return Ok(patch);
            }
            runtime.phase = SessionPhase::DirectoryUnavailable;
            replace_runtime(&mut patch.candidate, runtime)?;
        }
        OrganizationEvent::ControlChanged {
            pane,
            expected,
            replacement,
        } => {
            let Ok(mut runtime) = state.runtime(*pane) else {
                return Ok(patch);
            };
            if runtime.control != *expected {
                return Ok(patch);
            }
            if expected == replacement {
                return Ok(patch);
            }
            let next = runtime
                .control_generation
                .checked_successor()
                .ok_or(ReducerFailure::CounterExhausted)?;
            if let ControlRecord::Held { client, generation } = replacement
                && (client.server() != pane.server()
                    || client.local() > state.high_water.client
                    || *generation != next)
            {
                return Err(ReducerFailure::InvalidFacts);
            }
            runtime.control = *replacement;
            runtime.control_generation = next;
            replace_runtime(&mut patch.candidate, runtime)?;
            patch
                .effects
                .push(OrganizationEffect::CancelStartupIntents { pane: *pane });
        }
        OrganizationEvent::DisplayDirectoryObserved(observation) => {
            if !current_session(state, observation.session(), false) {
                return Ok(patch);
            }
            let pane = observation.session().pane;
            let observations = patch.candidate.observations.entry(pane).or_default();
            if observations
                .display
                .as_ref()
                .is_some_and(|old| old.sequence() >= observation.sequence())
            {
                return Ok(patch);
            }
            if observation.directory().bytes().len() > state.limits.directory_bytes {
                return Err(ReducerFailure::QuotaExceeded);
            }
            observations.display = Some(observation.clone());
            let mut org = state.organization().clone();
            let changed: Vec<_> = org
                .workspaces
                .iter()
                .filter(|w| {
                    w.pinned_directory.value.is_none()
                        && org
                            .tabs
                            .iter()
                            .any(|t| t.id == w.root_tab && t.root_pane == pane)
                })
                .map(|w| w.id)
                .collect();
            super::naming::refresh_workspace_names(&patch.candidate, &mut org, &changed)?;
            patch
                .candidate
                .replace_organization(org, state.snapshot.runtime().to_vec())?;
        }
        OrganizationEvent::RestorationDirectoryConfirmed(observation) => {
            if !current_session(state, observation.session(), false) {
                return Ok(patch);
            }
            let pane = observation.session().pane;
            let observations = patch.candidate.observations.entry(pane).or_default();
            if observations
                .confirmed_watermark
                .is_some_and(|old| old >= observation.sequence())
            {
                return Ok(patch);
            }
            if observation.directory().bytes().len() > state.limits.directory_bytes {
                return Err(ReducerFailure::QuotaExceeded);
            }
            observations.confirmed_watermark = Some(observation.sequence());
            let mut org = state.organization().clone();
            let record = org
                .panes
                .iter_mut()
                .find(|v| v.id == pane)
                .ok_or(ReducerFailure::InvalidFacts)?;
            if record.confirmed_restoration_directory.value.as_ref()
                != Some(observation.directory())
            {
                record.confirmed_restoration_directory.value =
                    Some(observation.directory().clone());
                record.confirmed_restoration_directory.version = record
                    .confirmed_restoration_directory
                    .version
                    .checked_successor()
                    .ok_or(ReducerFailure::CounterExhausted)?;
            }
            patch
                .candidate
                .replace_organization(org, state.snapshot.runtime().to_vec())?;
        }
        OrganizationEvent::CloseWorkObserved(observation) => {
            if !current_session(state, observation.session(), false) {
                return Ok(patch);
            }
            let observations = patch
                .candidate
                .observations
                .entry(observation.session().pane)
                .or_default();
            if let Some(old) = observations.close_work {
                if observation.sequence() <= old.sequence() {
                    return Ok(patch);
                }
                if observation.generation() < old.generation()
                    || observation.observed_at() < old.observed_at()
                    || (observation.generation() == old.generation()
                        && observation.state() != old.state())
                {
                    return Err(ReducerFailure::InvalidFacts);
                }
            }
            observations.close_work = Some(*observation);
        }
        OrganizationEvent::RetirementCleanup {
            batch,
            session,
            result,
        } => return plan_cleanup(state, *batch, *session, *result),
        OrganizationEvent::WorktreeOrganizationChanged(_) => {
            return Err(ReducerFailure::WrongReducer);
        }
    }
    Ok(patch)
}

fn current_session(state: &ReducerState, session: SessionKey, starting: bool) -> bool {
    session.epoch == state.epoch()
        && !session_pending(state, session)
        && state.runtime(session.pane).is_ok_and(|runtime| {
            runtime.session_generation == session.generation
                && (matches!(runtime.phase, SessionPhase::Running { .. })
                    || starting
                        && matches!(runtime.phase, SessionPhase::Starting { .. })
                        && state
                            .startup_reservations
                            .get(&session.pane)
                            .is_some_and(|reservation| reservation.session() == session))
        })
}
pub(crate) fn pane_pending(state: &ReducerState, pane: PaneId) -> bool {
    state.runtime_cleanups.contains_key(&pane)
        || state
            .retirements
            .values()
            .any(|v| v.panes().iter().any(|p| p.pane() == pane))
        || state
            .native_retirements
            .values()
            .any(|v| v.panes().iter().any(|p| p.pane() == pane))
}
fn session_pending(state: &ReducerState, session: SessionKey) -> bool {
    state
        .retirements
        .values()
        .any(|v| v.sessions().iter().any(|s| s.session() == session))
        || state
            .native_retirements
            .values()
            .any(|v| v.sessions().iter().any(|s| s.session() == session))
}
fn invalidation_effects(pane: PaneId, generation: SessionGeneration) -> Vec<OrganizationEffect> {
    vec![
        OrganizationEffect::CancelStartupIntents { pane },
        OrganizationEffect::InvalidatePaneRuntime {
            pane,
            generation,
            clear_content: true,
            clear_approvals: true,
        },
    ]
}
fn end_runtime(
    patch: &mut ReductionPatch,
    session: SessionKey,
    phase: SessionPhase,
) -> Result<(), ReducerFailure> {
    let mut runtime = patch.candidate.runtime(session.pane)?;
    runtime.session_generation = runtime
        .session_generation
        .checked_successor()
        .ok_or(ReducerFailure::CounterExhausted)?;
    runtime.phase = phase;
    patch.candidate.startup_reservations.remove(&session.pane);
    patch.candidate.observations.remove(&session.pane);
    patch.effects = invalidation_effects(session.pane, runtime.session_generation);
    replace_runtime(&mut patch.candidate, runtime)
}
fn replace_runtime(
    state: &mut ReducerState,
    replacement: PaneRuntimeRecord,
) -> Result<(), ReducerFailure> {
    let mut runtime = state.snapshot.runtime().to_vec();
    *runtime
        .iter_mut()
        .find(|v| v.pane == replacement.pane)
        .ok_or(ReducerFailure::InvalidFacts)? = replacement;
    let mut org = state.organization().clone();
    let mut facts = org
        .retained
        .as_ref()
        .ok_or(ReducerFailure::InvalidFacts)?
        .parts()
        .clone();
    *facts
        .pane_generations
        .iter_mut()
        .find(|v| v.pane() == replacement.pane)
        .ok_or(ReducerFailure::InvalidFacts)? = PaneGenerationHighWater::new(
        replacement.pane,
        replacement.session_generation,
        replacement.control_generation,
    );
    org.retained =
        Some(OrganizationRetainedFacts::new(facts).map_err(|_| ReducerFailure::InvalidFacts)?);
    state.replace_organization(org, runtime)
}

/// One epoch-local owner per terminated Pane, independent of phase or deletion.
/// Startup stays fenced until trusted cleanup. Accepted retirement transfers this
/// comparison into its durable descriptor; restart still needs native evidence.
#[derive(Clone, Copy)]
pub(crate) struct RuntimeCleanupOwner {
    pub(crate) pane: RetirementPane,
    session: RetiredSession,
}

fn retain_runtime_cleanup(
    state: &ReducerState,
    patch: &mut ReductionPatch,
    session: SessionKey,
    generation: SessionGeneration,
    reason: RetirementReason,
) -> Result<(), ReducerFailure> {
    if state.runtime_cleanups.len() >= MAX_RETIREMENT_SESSIONS {
        return Err(ReducerFailure::QuotaExceeded);
    }
    let pane = state
        .organization()
        .panes
        .iter()
        .find(|v| v.id == session.pane)
        .ok_or(ReducerFailure::InvalidFacts)?;
    let tab = state
        .organization()
        .tabs
        .iter()
        .find(|v| v.id == pane.tab)
        .ok_or(ReducerFailure::InvalidFacts)?;
    let owner = RuntimeCleanupOwner {
        pane: RetirementPane::new(tab.workspace, tab.id, pane.id, tab.worktree, generation)
            .map_err(retirement_error)?,
        session: RetiredSession::new(session, reason, RetirementCleanupState::AwaitingEvidence)
            .map_err(retirement_error)?,
    };
    patch.candidate.runtime_cleanups.insert(pane.id, owner);
    Ok(())
}

/// Capture the exact already-retiring Session set, including deleted hierarchy.
/// Shared owners retain their first reason and immutable terminal cleanup result.
pub(crate) fn retiring_basis(
    state: &ReducerState,
) -> Result<(Vec<RetirementPane>, Vec<RetiredSession>), ReducerFailure> {
    let mut panes: BTreeMap<_, _> = state
        .runtime_cleanups
        .iter()
        .map(|(pane, owner)| (*pane, owner.pane))
        .collect();
    let mut sessions: BTreeMap<_, _> = state
        .runtime_cleanups
        .values()
        .map(|owner| (owner.session.session(), owner.session))
        .collect();
    for descriptor in state
        .retirements
        .values()
        .map(RetirementRequest::Operation)
        .chain(
            state
                .native_retirements
                .values()
                .map(RetirementRequest::Native),
        )
    {
        let affected = match descriptor {
            RetirementRequest::Operation(v) => v.panes(),
            RetirementRequest::Native(v) => v.panes(),
        };
        for pane in affected {
            match panes.get(&pane.pane()) {
                Some(old) if old.invalidation_generation() >= pane.invalidation_generation() => {}
                _ => {
                    panes.insert(pane.pane(), *pane);
                }
            }
        }
        for session in descriptor.sessions() {
            if let Some(old) = sessions.insert(session.session(), *session)
                && old != *session
            {
                return Err(ReducerFailure::InvalidFacts);
            }
        }
    }
    Ok((
        panes.into_values().collect(),
        sessions.into_values().collect(),
    ))
}

pub(crate) fn plan_end_all(
    state: &ReducerState,
    handle: &AssessmentRef,
    context: &ReductionContext,
) -> Result<ReductionPatch, ReducerFailure> {
    plan_retirement(
        state,
        &AssessmentTarget::EndAllSessions(state.organization().server),
        handle,
        context,
    )
}

fn plan_retirement(
    state: &ReducerState,
    target: &AssessmentTarget,
    handle: &AssessmentRef,
    context: &ReductionContext,
) -> Result<ReductionPatch, ReducerFailure> {
    let validation = match target {
        AssessmentTarget::RemoveWorktree { scope, worktree } => {
            super::assessment::validate_worktree_retirement(
                state, *scope, *worktree, handle, context,
            )
        }
        _ => super::assessment::validate(state, &context.principal, handle, target, context.now),
    };
    let record = match validation {
        Ok(record) => record,
        Err(error) => return Ok(ReductionPatch::rejected(state, error.operation_failure())),
    };
    if record
        .retiring_sessions
        .iter()
        .any(|v| v.session().epoch != state.epoch())
    {
        return Ok(ReductionPatch::rejected(
            state,
            OperationFailure::BusyRetiring,
        ));
    }
    let completion = if matches!(target, AssessmentTarget::RemoveWorktree { .. }) {
        RetirementCompletion::ContinueGitSettlement
    } else {
        RetirementCompletion::AppliedAtAcceptanceRevision
    };
    let reason = if completion == RetirementCompletion::ContinueGitSettlement {
        RetirementReason::WorktreeRemoval
    } else {
        RetirementReason::EndAllSessions
    };
    let selected: BTreeSet<_> = record.panes.iter().map(|v| v.pane).collect();
    let FencedRetirement {
        mut patch,
        panes,
        sessions,
        fresh,
    } = fence_retirement(
        state,
        &selected,
        reason,
        matches!(target, AssessmentTarget::EndAllSessions(_)),
    )?;
    let descriptor = RetirementDescriptor::new(RetirementDescriptorParts {
        batch: state.next_retirement_batch()?,
        binding: context.binding(),
        assessment: *handle,
        acceptance_revision: acceptance_revision(state, &patch)?,
        target: *target,
        panes,
        sessions,
        completion,
        eventual_result: context
            .facts
            .retirement_outcome
            .as_ref()
            .ok_or(ReducerFailure::InvalidFacts)?
            .compact()
            .clone(),
    })
    .map_err(retirement_error)?;
    super::close::check_retirement_capacity(state, &descriptor)?;
    for session in fresh {
        patch.effects.push(OrganizationEffect::RetireSession {
            batch: descriptor.batch(),
            session,
            reason,
        });
    }
    patch.candidate.assessments.remove(&handle.operation);
    patch.candidate.retirement_sequence = descriptor.batch().sequence();
    patch
        .candidate
        .retirements
        .insert(descriptor.batch(), descriptor.clone());
    patch.resolution = Resolution::AwaitingRetirement(Box::new(RetirementPlan { descriptor }));
    Ok(patch)
}

fn acceptance_revision(
    state: &ReducerState,
    patch: &ReductionPatch,
) -> Result<OrganizationRevision, ReducerFailure> {
    if state.snapshot == patch.candidate.snapshot {
        Ok(state.organization().revision)
    } else {
        state
            .organization()
            .revision
            .checked_successor()
            .ok_or(ReducerFailure::CounterExhausted)
    }
}
fn retirement_error(error: RetirementRecordError) -> ReducerFailure {
    match error {
        RetirementRecordError::TooLarge => ReducerFailure::QuotaExceeded,
        _ => ReducerFailure::InvalidFacts,
    }
}
fn end_reason(reason: RetirementReason) -> Result<EndReason, ReducerFailure> {
    match reason {
        RetirementReason::EndAllSessions => Ok(EndReason::EndAllSessions),
        RetirementReason::Stop => Ok(EndReason::Stop),
        RetirementReason::ServerUpdate => Ok(EndReason::ServerUpdate),
        RetirementReason::WorktreeRemoval => Ok(EndReason::WorktreeRemoval),
        _ => Err(ReducerFailure::InvalidFacts),
    }
}

// Returns fresh process retirements separately from Sessions already owned by
// another batch. Every selected Pane is fenced before any process effect escapes.
struct FencedRetirement {
    patch: ReductionPatch,
    panes: Vec<RetirementPane>,
    sessions: Vec<RetiredSession>,
    fresh: Vec<SessionKey>,
}

fn fence_retirement(
    state: &ReducerState,
    selected: &BTreeSet<PaneId>,
    reason: RetirementReason,
    server_wide: bool,
) -> Result<FencedRetirement, ReducerFailure> {
    let (retiring_panes, retiring_sessions) = retiring_basis(state)?;
    let mut panes: BTreeMap<_, _> = retiring_panes
        .into_iter()
        .filter(|v| server_wide || selected.contains(&v.pane()))
        .map(|v| (v.pane(), v))
        .collect();
    let mut sessions: BTreeMap<_, _> = retiring_sessions
        .into_iter()
        .filter(|v| server_wide || selected.contains(&v.session().pane))
        .map(|v| (v.session(), v))
        .collect();
    // H0 descriptors bind one epoch. Old-instance cleanup must settle before a
    // new-epoch batch can be accepted; a restart is never cleanup evidence.
    if sessions.keys().any(|v| v.epoch != state.epoch()) {
        return Err(ReducerFailure::InvalidFacts);
    }
    let mut patch = ReductionPatch::unchanged(state, Outcome::Applied);
    patch
        .candidate
        .runtime_cleanups
        .retain(|pane, _| !server_wide && !selected.contains(pane));
    let mut fresh = Vec::new();
    let org = state.organization();
    let mut runtime_records = state.snapshot.runtime().to_vec();
    let mut facts = org
        .retained
        .as_ref()
        .ok_or(ReducerFailure::InvalidFacts)?
        .parts()
        .clone();
    let tabs: BTreeMap<_, _> = org.tabs.iter().map(|v| (v.id, v)).collect();
    for (pane, runtime) in org.panes.iter().zip(&mut runtime_records) {
        if !selected.contains(&pane.id) {
            continue;
        }
        if runtime.pane != pane.id {
            return Err(ReducerFailure::InvalidFacts);
        }
        let tab = tabs.get(&pane.tab).ok_or(ReducerFailure::InvalidFacts)?;
        let session = SessionKey {
            pane: pane.id,
            epoch: state.epoch(),
            generation: runtime.session_generation,
        };
        if matches!(
            runtime.phase,
            SessionPhase::Running { .. } | SessionPhase::Starting { .. }
        ) && !sessions.contains_key(&session)
        {
            fresh.push(session);
            sessions.insert(
                session,
                RetiredSession::new(session, reason, RetirementCleanupState::AwaitingEvidence)
                    .map_err(retirement_error)?,
            );
        }
        runtime.session_generation = runtime
            .session_generation
            .checked_successor()
            .ok_or(ReducerFailure::CounterExhausted)?;
        runtime.phase = SessionPhase::Ended {
            reason: end_reason(reason)?,
        };
        panes.insert(
            pane.id,
            RetirementPane::new(
                tab.workspace,
                tab.id,
                pane.id,
                tab.worktree,
                runtime.session_generation,
            )
            .map_err(retirement_error)?,
        );
        patch.candidate.startup_reservations.remove(&pane.id);
        patch.candidate.observations.remove(&pane.id);
        patch
            .effects
            .extend(invalidation_effects(pane.id, runtime.session_generation));
    }
    if sessions.len() > MAX_RETIREMENT_SESSIONS || panes.len() > MAX_RETIREMENT_PANES {
        return Err(ReducerFailure::QuotaExceeded);
    }
    for (mark, runtime) in facts.pane_generations.iter_mut().zip(&runtime_records) {
        if mark.pane() != runtime.pane {
            return Err(ReducerFailure::InvalidFacts);
        }
        *mark = PaneGenerationHighWater::new(
            runtime.pane,
            runtime.session_generation,
            runtime.control_generation,
        );
    }
    let mut org = org.clone();
    org.retained =
        Some(OrganizationRetainedFacts::new(facts).map_err(|_| ReducerFailure::InvalidFacts)?);
    patch.candidate.replace_organization(org, runtime_records)?;
    Ok(FencedRetirement {
        patch,
        panes: panes.into_values().collect(),
        sessions: sessions.into_values().collect(),
        fresh,
    })
}

fn cleanup_state(result: CleanupResult) -> RetirementCleanupState {
    match result {
        CleanupResult::Confirmed(class) => RetirementCleanupState::Confirmed(class),
        CleanupResult::Failed(class) => RetirementCleanupState::Failed(class),
    }
}

fn plan_cleanup(
    state: &ReducerState,
    batch: RetirementBatchId,
    session: SessionKey,
    result: CleanupResult,
) -> Result<ReductionPatch, ReducerFailure> {
    let target = state
        .retirements
        .get(&batch)
        .and_then(|v| v.sessions().iter().find(|v| v.session() == session))
        .or_else(|| {
            state
                .native_retirements
                .get(&batch)
                .and_then(|v| v.sessions().iter().find(|v| v.session() == session))
        });
    let Some(target) = target else {
        return Ok(ReductionPatch::unchanged(state, Outcome::Applied));
    };
    plan_session_cleanup(state, *target, result)
}

fn plan_session_cleanup(
    state: &ReducerState,
    target: RetiredSession,
    result: CleanupResult,
) -> Result<ReductionPatch, ReducerFailure> {
    let mut patch = ReductionPatch::unchanged(state, Outcome::Applied);
    if target.cleanup() != RetirementCleanupState::AwaitingEvidence {
        return Ok(patch);
    }
    let session = target.session();
    if let Some(owner) = patch.candidate.runtime_cleanups.get_mut(&session.pane) {
        if owner.session != target {
            return Err(ReducerFailure::InvalidFacts);
        }
        owner.session = RetiredSession::new(session, target.first_reason(), cleanup_state(result))
            .map_err(retirement_error)?;
        if matches!(result, CleanupResult::Confirmed(_)) {
            patch.candidate.runtime_cleanups.remove(&session.pane);
        }
    }
    let mut operations = Vec::new();
    let mut native = Vec::new();
    for (id, old) in &state.retirements {
        if !old.sessions().iter().any(|v| v.session() == session) {
            continue;
        }
        let mut parts = old.parts().clone();
        update_cleanup(&mut parts.sessions, session, target.first_reason(), result)?;
        let next = RetirementDescriptor::new(parts).map_err(retirement_error)?;
        patch.candidate.retirements.insert(*id, next.clone());
        operations.push(next);
    }
    for (id, old) in &state.native_retirements {
        if !old.sessions().iter().any(|v| v.session() == session) {
            continue;
        }
        let mut parts = old.parts().clone();
        update_cleanup(&mut parts.sessions, session, target.first_reason(), result)?;
        let next = NativeRetirementDescriptor::new(parts).map_err(retirement_error)?;
        patch
            .operations
            .push(crate::storage::OperationWrite::NativeRetirement(
                NativeRetirementMutation::Progress {
                    expected: Box::new(old.clone()),
                    next: Box::new(next.clone()),
                },
            ));
        patch.candidate.native_retirements.insert(*id, next.clone());
        native.push(next);
    }
    // Cleanup cannot change membership or overwrite a newer ended phase/reason.
    if !operations.is_empty() || !native.is_empty() {
        patch.resolution = Resolution::RetirementProgress { operations, native };
    }
    Ok(patch)
}
fn update_cleanup(
    sessions: &mut [RetiredSession],
    session: SessionKey,
    reason: RetirementReason,
    result: CleanupResult,
) -> Result<(), ReducerFailure> {
    let old = sessions
        .iter_mut()
        .find(|v| v.session() == session)
        .ok_or(ReducerFailure::InvalidFacts)?;
    if old.first_reason() != reason || old.cleanup() != RetirementCleanupState::AwaitingEvidence {
        return Err(ReducerFailure::InvalidFacts);
    }
    *old = RetiredSession::new(session, reason, cleanup_state(result)).map_err(retirement_error)?;
    Ok(())
}

impl super::OrganizationReducer {
    /// Native authorization and durable request admission belong to the provider.
    /// This records its exact identity without inventing a pairing or assessment.
    pub fn prepare_native_retirement(
        &self,
        request: crate::native_management::NativeManagementRequestRef,
        action: crate::native_management::NativeRetirementAction,
        _now: ReducerTick,
    ) -> Result<super::PreparedReduction, ReducerFailure> {
        self.state.check_admission()?;
        let state = &self.state;
        if request.server() != state.organization().server {
            return Err(ReducerFailure::WrongServer);
        }
        if state
            .native_retirements
            .values()
            .any(|v| v.request().id_bytes() == request.id_bytes())
        {
            return Err(ReducerFailure::InvalidFacts);
        }
        let selected = state.organization().panes.iter().map(|v| v.id).collect();
        let FencedRetirement {
            mut patch,
            panes,
            sessions,
            fresh,
        } = fence_retirement(state, &selected, action.reason(), true)?;
        let descriptor = NativeRetirementDescriptor::new(NativeRetirementDescriptorParts {
            request,
            batch: state.next_retirement_batch()?,
            action,
            acceptance_revision: acceptance_revision(state, &patch)?,
            panes,
            sessions,
        })
        .map_err(retirement_error)?;
        check_native_capacity(state, &descriptor)?;
        patch.candidate.stopped = true;
        patch.candidate.retirement_sequence = descriptor.batch().sequence();
        patch
            .candidate
            .native_retirements
            .insert(descriptor.batch(), descriptor.clone());
        for session in fresh {
            patch.effects.push(OrganizationEffect::RetireSession {
                batch: descriptor.batch(),
                session,
                reason: action.reason(),
            });
        }
        patch
            .operations
            .push(crate::storage::OperationWrite::NativeRetirement(
                NativeRetirementMutation::Accept(Box::new(descriptor.clone())),
            ));
        patch.resolution = Resolution::AwaitingNativeRetirement(Box::new(descriptor));
        self.prepare_patch(patch, None)
    }

    /// Final completion is store-only and must share the ledger completion commit.
    /// Git retires non-deletingly and completes only through its final membership
    /// settlement, so this entry point cannot complete a Git operation early.
    pub fn prepare_retirement_completion(
        &self,
        batch: RetirementBatchId,
    ) -> Result<super::PreparedReduction, ReducerFailure> {
        self.state.check_admission()?;
        let state = &self.state;
        let mut patch = ReductionPatch::unchanged(state, Outcome::Applied);
        if let Some(descriptor) = state.retirements.get(&batch) {
            if !descriptor.cleanup_confirmed() {
                return Err(ReducerFailure::InvalidFacts);
            }
            if descriptor.completion() == RetirementCompletion::ContinueGitSettlement {
                return Err(ReducerFailure::WrongReducer);
            }
            patch.candidate.retirements.remove(&batch);
            patch.resolution = Resolution::RetirementCompleted(Box::new(RetirementPlan {
                descriptor: descriptor.clone(),
            }));
        } else if let Some(descriptor) = state.native_retirements.get(&batch) {
            if !descriptor.cleanup_confirmed() {
                return Err(ReducerFailure::InvalidFacts);
            }
            patch.candidate.native_retirements.remove(&batch);
            patch.candidate.stopped = true;
            patch
                .operations
                .push(crate::storage::OperationWrite::NativeRetirement(
                    NativeRetirementMutation::Complete {
                        request: descriptor.request(),
                        batch,
                    },
                ));
            patch.resolution = Resolution::NativeRetirementCompleted(Box::new(descriptor.clone()));
        } else {
            return Err(ReducerFailure::TargetNotFound);
        }
        self.prepare_patch(patch, None)
    }

    pub fn prepare_worktree_assessment(
        &self,
        scope: RepositoryScope,
        worktree: WorktreeId,
        context: &ReductionContext,
    ) -> Result<super::PreparedReduction, ReducerFailure> {
        self.state.check_admission()?;
        self.state.check_principal(context)?;
        let patch = match super::assessment::plan_assessment(
            &self.state,
            &AssessmentTarget::RemoveWorktree { scope, worktree },
            context,
        ) {
            Err(ReducerFailure::QuotaExceeded) => {
                ReductionPatch::rejected(&self.state, OperationFailure::Capacity)
            }
            other => other?,
        };
        self.prepare_patch(patch, Some(context.binding()))
    }

    /// Internal Git acceptance retains membership. Git owns external assessment,
    /// intent phases and execution; this compares only exact terminal hierarchy.
    pub fn prepare_worktree_retirement(
        &self,
        scope: RepositoryScope,
        worktree: WorktreeId,
        handle: &AssessmentRef,
        context: &ReductionContext,
    ) -> Result<super::PreparedReduction, ReducerFailure> {
        self.state.check_admission()?;
        self.state.check_principal(context)?;
        let patch = match plan_retirement(
            &self.state,
            &AssessmentTarget::RemoveWorktree { scope, worktree },
            handle,
            context,
        ) {
            Err(ReducerFailure::QuotaExceeded) => {
                ReductionPatch::rejected(&self.state, OperationFailure::Capacity)
            }
            other => other?,
        };
        self.prepare_patch(patch, Some(context.binding()))
    }
}

fn native_charge(descriptor: &NativeRetirementDescriptor) -> Result<usize, ReducerFailure> {
    descriptor
        .panes()
        .len()
        .checked_mul(112)
        .and_then(|n| {
            descriptor
                .sessions()
                .len()
                .checked_mul(64)
                .and_then(|s| n.checked_add(s))
        })
        .and_then(|n| n.checked_add(320))
        .ok_or(ReducerFailure::QuotaExceeded)
}
fn check_native_capacity(
    state: &ReducerState,
    descriptor: &NativeRetirementDescriptor,
) -> Result<(), ReducerFailure> {
    if state
        .retirements
        .len()
        .checked_add(state.native_retirements.len())
        .is_none_or(|n| n >= state.limits.retirement_batches)
        || state.native_retirements.len() >= MAX_NATIVE_RETIREMENTS
    {
        return Err(ReducerFailure::QuotaExceeded);
    }
    let mut bytes = native_charge(descriptor)?;
    for old in state.retirements.values() {
        bytes = bytes
            .checked_add(old.value_bytes().map_err(retirement_error)?)
            .ok_or(ReducerFailure::QuotaExceeded)?;
    }
    for old in state.native_retirements.values() {
        bytes = bytes
            .checked_add(native_charge(old)?)
            .ok_or(ReducerFailure::QuotaExceeded)?;
    }
    if bytes > state.limits.retirement_bytes {
        return Err(ReducerFailure::QuotaExceeded);
    }
    Ok(())
}

pub(crate) fn validate_retirement_recovery(
    retained: &super::RetainedOrganization,
) -> Result<(), ReducerFailure> {
    let invalid = ReducerFailure::InvalidDurableState(DurableInvariant::Retirement);
    let marks = retained
        .organization()
        .retained
        .as_ref()
        .ok_or(invalid)?
        .pane_generations();
    let marks: BTreeMap<_, _> = marks.iter().map(|v| (v.pane(), v.session())).collect();
    let mut sessions = BTreeMap::new();
    let mut native_ids = BTreeSet::new();
    let mut pane_membership = BTreeMap::new();
    let surviving_panes: BTreeMap<_, _> = retained
        .organization()
        .panes
        .iter()
        .map(|v| (v.id, v.tab))
        .collect();
    let surviving_tabs: BTreeMap<_, _> = retained
        .organization()
        .tabs
        .iter()
        .map(|v| (v.id, (v.workspace, v.worktree)))
        .collect();
    for descriptor in retained
        .pending_retirements()
        .iter()
        .map(RetirementRequest::Operation)
        .chain(
            retained
                .native_retirements()
                .iter()
                .map(RetirementRequest::Native),
        )
    {
        let panes = match descriptor {
            RetirementRequest::Operation(v) => v.panes(),
            RetirementRequest::Native(v) => {
                if !native_ids.insert(v.request().id_bytes()) {
                    return Err(invalid);
                }
                v.panes()
            }
        };
        for pane in panes {
            let membership = (pane.workspace(), pane.tab(), pane.worktree());
            if pane_membership
                .insert(pane.pane(), membership)
                .is_some_and(|old| old != membership)
                || surviving_panes.get(&pane.pane()).is_some_and(|tab| {
                    *tab != pane.tab()
                        || surviving_tabs.get(tab) != Some(&(pane.workspace(), pane.worktree()))
                })
            {
                return Err(invalid);
            }
            if marks
                .get(&pane.pane())
                .is_some_and(|v| *v < pane.invalidation_generation())
            {
                return Err(invalid);
            }
        }
        for session in descriptor.sessions() {
            if let Some(old) = sessions.insert(session.session(), *session)
                && old != *session
            {
                return Err(invalid);
            }
        }
    }
    Ok(())
}

impl super::OrganizationReducer {
    /// Reduces the owner's retained terminal outcome, independently of cleanup.
    /// Explicit retirement is never interpreted as ordinary exit, even at EOF.
    pub fn prepare_terminal_result(
        &self,
        terminal: crate::runtime_authority::SessionTerminalResult,
    ) -> Result<super::PreparedReduction, ReducerFailure> {
        use crate::runtime_authority::RuntimeTerminalStatus;
        self.state.check_admission()?;
        let session = terminal.session();
        let event = match terminal.status() {
            RuntimeTerminalStatus::OrdinaryExit { exit_status } => {
                OrganizationEvent::OrdinaryExit {
                    session,
                    class: match exit_status {
                        Some(0) => OrdinaryExitClass::Success,
                        Some(_) => OrdinaryExitClass::Nonzero,
                        None => OrdinaryExitClass::Signal,
                    },
                }
            }
            RuntimeTerminalStatus::RuntimeFailed { class } => {
                OrganizationEvent::RuntimeFailed { session, class }
            }
            RuntimeTerminalStatus::StartupFailed { class }
                if self
                    .state
                    .runtime(session.pane)
                    .is_ok_and(|v| matches!(v.phase, SessionPhase::Starting { .. })) =>
            {
                OrganizationEvent::RuntimeFailed { session, class }
            }
            RuntimeTerminalStatus::StartupFailed { .. } | RuntimeTerminalStatus::Retired { .. } => {
                return self.prepare_patch(
                    ReductionPatch::unchanged(&self.state, Outcome::Applied),
                    None,
                );
            }
        };
        self.prepare_event(&event)
    }

    /// Accept the owner's independent cleanup, even before a destructive batch.
    /// Composition supplies trusted evidence, normally RuntimeCompletion::cleanup;
    /// terminal failure, EOF, missing handles and restart are not cleanup proof.
    /// The same observation advances every descriptor awaiting this exact owner.
    pub fn prepare_runtime_cleanup(
        &self,
        observation: SessionCleanupObservation,
    ) -> Result<super::PreparedReduction, ReducerFailure> {
        self.state.check_admission()?;
        let session = observation.session();
        let target = self
            .state
            .runtime_cleanups
            .get(&session.pane)
            .map(|owner| owner.session)
            .filter(|owner| owner.session() == session)
            .or_else(|| {
                self.state
                    .retirements
                    .values()
                    .flat_map(|v| v.sessions())
                    .chain(
                        self.state
                            .native_retirements
                            .values()
                            .flat_map(|v| v.sessions()),
                    )
                    .find(|v| v.session() == session)
                    .copied()
            });
        let patch = match target {
            Some(target) => {
                if target.first_reason() != observation.first_reason() {
                    return Err(ReducerFailure::InvalidFacts);
                }
                plan_session_cleanup(&self.state, target, observation.result())?
            }
            None => ReductionPatch::unchanged(&self.state, Outcome::Applied),
        };
        self.prepare_patch(patch, None)
    }

    /// Composition supplies trusted cleanup evidence, including previous-instance
    /// cleanup. The comparison record itself is not native proof.
    pub fn prepare_cleanup_observation(
        &self,
        batch: RetirementBatchId,
        observation: SessionCleanupObservation,
    ) -> Result<super::PreparedReduction, ReducerFailure> {
        self.state.check_admission()?;
        let session = observation.session();
        let target = self
            .state
            .retirements
            .get(&batch)
            .and_then(|v| v.sessions().iter().find(|v| v.session() == session))
            .or_else(|| {
                self.state
                    .native_retirements
                    .get(&batch)
                    .and_then(|v| v.sessions().iter().find(|v| v.session() == session))
            });
        if target.is_some_and(|v| v.first_reason() != observation.first_reason()) {
            return Err(ReducerFailure::InvalidFacts);
        }
        self.prepare_event(&OrganizationEvent::RetirementCleanup {
            batch,
            session,
            result: observation.result(),
        })
    }

    /// Service-owned Git failure/reconciliation releases non-deleting retirement.
    /// The caller atomically commits its final Git intent and ledger result with
    /// this reduction. Cleanup alone cannot call ordinary completion or succeed.
    pub fn prepare_worktree_retirement_release(
        &self,
        batch: RetirementBatchId,
        binding: OperationBinding,
    ) -> Result<super::PreparedReduction, ReducerFailure> {
        self.state.check_admission()?;
        let descriptor = self
            .state
            .retirements
            .get(&batch)
            .ok_or(ReducerFailure::TargetNotFound)?;
        if descriptor.binding() != binding
            || descriptor.completion() != RetirementCompletion::ContinueGitSettlement
            || !descriptor.cleanup_confirmed()
        {
            return Err(ReducerFailure::InvalidFacts);
        }
        let mut patch = ReductionPatch::unchanged(&self.state, Outcome::Applied);
        patch.candidate.retirements.remove(&batch);
        if patch
            .candidate
            .git_allocations
            .remove(&binding.id)
            .is_some()
        {
            patch
                .operations
                .push(crate::storage::OperationWrite::GitAllocation(
                    crate::worktrees::intent::GitAllocationMutation::Abandon { binding },
                ));
        }
        patch.resolution = Resolution::GitRetirementReleased(Box::new(RetirementPlan {
            descriptor: descriptor.clone(),
        }));
        self.prepare_patch(patch, None)
    }
}

impl super::OrganizationReducer {
    /// Retire the exact hierarchy of a durable Started Git intent without deleting
    /// membership. The actor has already compared runtime work behind its barrier.
    pub fn prepare_started_git_retirement(
        &self,
        intent: &crate::worktrees::GitWriteIntent,
        context: &ReductionContext,
    ) -> Result<super::PreparedReduction, ReducerFailure> {
        use crate::worktrees::{GitIntentPhase, RetainedGitAction};
        self.state.check_admission()?;
        if intent.phase() != GitIntentPhase::Started
            || intent.binding() != context.binding()
            || !self
                .state
                .git_allocations
                .contains_key(&intent.binding().id)
        {
            return Err(ReducerFailure::InvalidFacts);
        }
        let RetainedGitAction::Remove { worktree, .. } = intent.parts().action else {
            return Err(ReducerFailure::InvalidFacts);
        };
        let scope = intent.parts().affected.parts();
        let org = self.state.organization();
        if scope
            .tabs
            .iter()
            .any(|tab| org.tabs.iter().find(|v| v.id == tab.id) != Some(tab))
            || scope.membership_versions.iter().any(|version| {
                !org.retained
                    .as_ref()
                    .is_some_and(|facts| facts.tab_membership_versions().contains(version))
            })
        {
            return Err(ReducerFailure::InvalidFacts);
        }
        let (previous_panes, previous_sessions) = retiring_basis(&self.state)?;
        let mut fresh = BTreeSet::new();
        for pane in &scope.panes {
            let current = self.state.runtime(pane.hierarchy.pane)?;
            // Started already fenced input and bound this Session. Revocation may
            // release its grant before retirement without replacing the owner.
            let same = current.session_generation == pane.hierarchy.session_generation;
            let transferred = pane.session.is_some_and(|session| {
                previous_sessions.iter().any(|v| v.session() == session)
                    && current.session_generation > session.generation
                    && matches!(current.phase, SessionPhase::Ended { .. })
            });
            if !same && !transferred {
                return Err(ReducerFailure::InvalidFacts);
            }
            if same {
                fresh.insert(pane.hierarchy.pane);
            }
        }
        let FencedRetirement {
            mut patch,
            mut panes,
            mut sessions,
            fresh: effects,
        } = fence_retirement(
            &self.state,
            &fresh,
            RetirementReason::WorktreeRemoval,
            false,
        )?;
        // End All or Stop may have invalidated an original Session after Started.
        // Transfer its first cleanup reason without replacing the later ended phase.
        for pane in &scope.panes {
            if fresh.contains(&pane.hierarchy.pane) {
                continue;
            }
            let previous = previous_panes
                .iter()
                .find(|v| v.pane() == pane.hierarchy.pane)
                .ok_or(ReducerFailure::InvalidFacts)?;
            panes.push(*previous);
            if let Some(session) = pane.session {
                sessions.push(
                    *previous_sessions
                        .iter()
                        .find(|v| v.session() == session)
                        .ok_or(ReducerFailure::InvalidFacts)?,
                );
            }
        }
        panes.sort_by_key(|v| v.pane());
        sessions.sort_by_key(|v| v.session());
        let descriptor = RetirementDescriptor::new(RetirementDescriptorParts {
            batch: self.state.next_retirement_batch()?,
            binding: intent.binding(),
            assessment: AssessmentRef {
                operation: intent.binding().id,
                digest: intent.binding().digest,
            },
            acceptance_revision: acceptance_revision(&self.state, &patch)?,
            target: AssessmentTarget::RemoveWorktree {
                scope: intent.parts().repository.scope(),
                worktree,
            },
            panes,
            sessions,
            completion: RetirementCompletion::ContinueGitSettlement,
            eventual_result: context
                .facts
                .retirement_outcome
                .as_ref()
                .ok_or(ReducerFailure::InvalidFacts)?
                .compact()
                .clone(),
        })
        .map_err(retirement_error)?;
        super::close::check_retirement_capacity(&self.state, &descriptor)?;
        for session in effects {
            patch.effects.push(OrganizationEffect::RetireSession {
                batch: descriptor.batch(),
                session,
                reason: RetirementReason::WorktreeRemoval,
            });
        }
        patch.candidate.retirement_sequence = descriptor.batch().sequence();
        patch
            .candidate
            .retirements
            .insert(descriptor.batch(), descriptor.clone());
        patch.resolution = Resolution::AwaitingRetirement(Box::new(RetirementPlan { descriptor }));
        self.prepare_patch(patch, Some(intent.binding()))
    }

    /// Consume the original allocation and release Git retirement in the same
    /// candidate as final membership. This grants no startup authority.
    pub fn prepare_git_settlement(
        &self,
        intent: &crate::worktrees::GitWriteIntent,
        result: &crate::worktrees::RetainedExternalResult,
        creator_still_authorized: bool,
    ) -> Result<super::PreparedReduction, ReducerFailure> {
        use crate::worktrees::{
            ExternalDisposition, GitAllocationMutation, GitIntentPhase, RetainedGitAction,
        };
        self.state.check_admission()?;
        let binding = intent.binding();
        let allocation = self
            .state
            .git_allocations
            .get(&binding.id)
            .filter(|value| value.binding() == binding)
            .ok_or(ReducerFailure::InvalidFacts)?;
        if self
            .state
            .retirements
            .values()
            .any(|v| v.binding() == binding && !v.cleanup_confirmed())
        {
            return Err(ReducerFailure::InvalidFacts);
        }
        let success = result.disposition == ExternalDisposition::DesiredStateObserved
            && result.failure.is_none();
        let mut patch = if success {
            if !matches!(
                intent.phase(),
                GitIntentPhase::Started | GitIntentPhase::ExternalFinished
            ) {
                return Err(ReducerFailure::InvalidFacts);
            }
            let org = self.state.organization();
            let (record, remove_tabs) = match &intent.parts().action {
                RetainedGitAction::Create {
                    worktree,
                    directory,
                    ..
                } => (
                    WorktreeRecord {
                        id: *worktree,
                        repository: intent.parts().repository.scope().repository,
                        directory: directory.clone(),
                        main: false,
                        missing: false,
                    },
                    vec![],
                ),
                RetainedGitAction::Remove {
                    worktree,
                    directory,
                    ..
                } => (
                    WorktreeRecord {
                        id: *worktree,
                        repository: intent.parts().repository.scope().repository,
                        directory: directory.clone(),
                        main: false,
                        missing: true,
                    },
                    intent.parts().affected.parts().tabs.clone(),
                ),
            };
            let version = org
                .retained
                .as_ref()
                .ok_or(ReducerFailure::InvalidFacts)?
                .repository_inventory_versions()
                .iter()
                .find(|v| v.id() == record.repository)
                .map(|v| v.version());
            super::creation::plan_worktree_patch(
                &self.state,
                &WorktreeOrganizationPatch {
                    binding,
                    scope: intent.parts().repository.scope(),
                    expected_inventory_version: version,
                    repositories: vec![],
                    worktrees: vec![(
                        record,
                        matches!(intent.parts().action, RetainedGitAction::Create { .. }),
                    )],
                    memberships: vec![],
                    settlement: Some(GitOrganizationSettlement {
                        reservation: allocation.clone(),
                        phase: intent.phase(),
                        remove_tabs,
                        creator_still_authorized,
                    }),
                },
            )?
        } else {
            let mut patch = ReductionPatch::unchanged(&self.state, Outcome::Applied);
            patch
                .candidate
                .retirements
                .retain(|_, value| value.binding() != binding);
            patch.candidate.git_allocations.remove(&binding.id);
            patch
                .operations
                .push(crate::storage::OperationWrite::GitAllocation(
                    GitAllocationMutation::Abandon { binding },
                ));
            patch
        };
        if matches!(patch.resolution, Resolution::Completed(Outcome::Failed(_))) {
            return self.prepare_patch(patch, Some(binding));
        }
        patch.resolution = Resolution::Completed(Outcome::Applied);
        self.prepare_patch(patch, Some(binding))
    }
}
