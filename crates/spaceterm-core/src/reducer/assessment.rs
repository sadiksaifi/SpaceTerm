use std::collections::BTreeSet;

use thiserror::Error;

use super::OrganizationReducer;
use super::contracts::*;
use super::state::{ReducerState, ReductionPatch};
use crate::authority::SessionKey;
use crate::identity::{PaneId, TabId};
use crate::operations::*;
use crate::organization::SessionPhase;

/// A comparison failure grants no destructive authority. The handle itself is
/// neither authentication nor evidence of native cleanup or Git state.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum AssessmentFailure {
    #[error("assessment comparison is stale")]
    StaleAssessment,
    #[error("assessment deadline has elapsed")]
    AssessmentExpired,
    #[error("assessment target no longer exists")]
    TargetNotFound,
    #[error("target is reserved by pending retirement or Git settlement")]
    BusyRetiring,
    #[error("validated recovery is required")]
    RecoveryRequired,
}
impl AssessmentFailure {
    pub(crate) const fn operation_failure(self) -> OperationFailure {
        match self {
            Self::StaleAssessment | Self::RecoveryRequired => OperationFailure::StaleAssessment,
            Self::AssessmentExpired => OperationFailure::AssessmentExpired,
            Self::TargetNotFound => OperationFailure::TargetDeleted,
            Self::BusyRetiring => OperationFailure::BusyRetiring,
        }
    }
}

impl OrganizationReducer {
    pub fn validate_assessment(
        &self,
        principal: &MutationPrincipal,
        handle: &AssessmentRef,
        intent: &AssessmentTarget,
        now: ReducerTick,
    ) -> Result<(), AssessmentFailure> {
        validate(&self.state, principal, handle, intent, now).map(|_| ())
    }
}

pub(crate) fn plan_assessment(
    state: &ReducerState,
    intent: &AssessmentTarget,
    context: &ReductionContext,
) -> Result<ReductionPatch, ReducerFailure> {
    // The ledger must intercept historical Assess retries. Reaching the reducer
    // twice with the same operation cannot refresh its deadline or authority.
    if state.assessments.contains_key(&context.operation) {
        return Err(ReducerFailure::InvalidFacts);
    }
    let reference = AssessmentRef {
        operation: context.operation,
        digest: context.request_digest,
    };
    let mut record = match capture(state, *intent, reference, context.principal, context.now) {
        Ok(record) => record,
        Err(ReducerFailure::TargetNotFound) => {
            return Ok(ReductionPatch::rejected(
                state,
                OperationFailure::TargetDeleted,
            ));
        }
        Err(ReducerFailure::WrongMembership) => {
            return Ok(ReductionPatch::rejected(
                state,
                OperationFailure::WrongMembership,
            ));
        }
        Err(error) => return Err(error),
    };
    if busy(state, &record, None) {
        return Ok(ReductionPatch::rejected(
            state,
            OperationFailure::BusyRetiring,
        ));
    }
    record.expires_at = ReducerTick::new(
        context
            .now
            .get()
            .checked_add(state.limits.assessment_lifetime_ms)
            .ok_or(ReducerFailure::CounterExhausted)?,
    );
    let mut count = 1usize;
    let mut client_count = 1usize;
    let mut bytes = record.value_bytes;
    for retained in state
        .assessments
        .values()
        .filter(|v| context.now < v.expires_at)
    {
        count = count.checked_add(1).ok_or(ReducerFailure::QuotaExceeded)?;
        client_count = client_count
            .checked_add(usize::from(
                retained.principal.client == context.principal.client,
            ))
            .ok_or(ReducerFailure::QuotaExceeded)?;
        bytes = bytes
            .checked_add(retained.value_bytes)
            .ok_or(ReducerFailure::QuotaExceeded)?;
    }
    if count > state.limits.assessments
        || client_count > state.limits.assessments_per_client
        || bytes > state.limits.assessment_bytes
    {
        return Err(ReducerFailure::QuotaExceeded);
    }
    let summary = summary(&record)?;
    let result = CloseAssessment::new(record.reference, record.epoch, summary)
        .map_err(|_| ReducerFailure::InvalidFacts)?;
    let mut patch = ReductionPatch::unchanged(state, Outcome::Assessment(Box::new(result)));
    patch
        .candidate
        .assessments
        .retain(|_, v| context.now < v.expires_at);
    patch
        .candidate
        .assessments
        .insert(context.operation, record);
    Ok(patch)
}

pub(crate) fn validate<'a>(
    state: &'a ReducerState,
    principal: &MutationPrincipal,
    handle: &AssessmentRef,
    intent: &AssessmentTarget,
    now: ReducerTick,
) -> Result<&'a AssessmentRecord, AssessmentFailure> {
    validate_with_reservation(state, principal, handle, intent, now, None)
}

/// The actor has already recorded Git's Started intent. Its exact durable
/// reservation must not block its own retirement; ordinary close has no exemption.
pub(crate) fn validate_worktree_retirement<'a>(
    state: &'a ReducerState,
    scope: RepositoryScope,
    worktree: crate::identity::WorktreeId,
    handle: &AssessmentRef,
    context: &ReductionContext,
) -> Result<&'a AssessmentRecord, AssessmentFailure> {
    validate_with_reservation(
        state,
        &context.principal,
        handle,
        &AssessmentTarget::RemoveWorktree { scope, worktree },
        context.now,
        Some(context.binding()),
    )
}

fn validate_with_reservation<'a>(
    state: &'a ReducerState,
    principal: &MutationPrincipal,
    handle: &AssessmentRef,
    intent: &AssessmentTarget,
    now: ReducerTick,
    reservation_owner: Option<OperationBinding>,
) -> Result<&'a AssessmentRecord, AssessmentFailure> {
    if state.suspended {
        return Err(AssessmentFailure::RecoveryRequired);
    }
    let record = state
        .assessments
        .get(&handle.operation)
        .ok_or(AssessmentFailure::StaleAssessment)?;
    if record.reference != *handle
        || record.principal != *principal
        || record.epoch != state.epoch()
        || record.target != *intent
        || now < record.captured_at
    {
        return Err(AssessmentFailure::StaleAssessment);
    }
    if now >= record.expires_at {
        return Err(AssessmentFailure::AssessmentExpired);
    }
    let current =
        capture(state, *intent, *handle, *principal, now).map_err(|failure| match failure {
            ReducerFailure::TargetNotFound => AssessmentFailure::TargetNotFound,
            _ => AssessmentFailure::StaleAssessment,
        })?;
    if record.workspaces != current.workspaces
        || record.tabs != current.tabs
        || record.tab_panes != current.tab_panes
        || record.retiring_panes != current.retiring_panes
        || record
            .retiring_sessions
            .iter()
            .map(|v| (v.session(), v.first_reason()))
            .ne(current
                .retiring_sessions
                .iter()
                .map(|v| (v.session(), v.first_reason())))
        || record.panes.len() != current.panes.len()
        || record.panes.iter().zip(&current.panes).any(|(old, new)| {
            old.pane != new.pane
                || old.tab != new.tab
                || old.runtime != new.runtime
                || old.close_work.map(|v| v.generation()) != new.close_work.map(|v| v.generation())
                || work_state(old, record.captured_at, state) != work_state(new, now, state)
        })
    {
        return Err(AssessmentFailure::StaleAssessment);
    }
    if busy(state, &current, reservation_owner) {
        return Err(AssessmentFailure::BusyRetiring);
    }
    Ok(record)
}

pub(crate) fn live_session(pane: &PaneAssessmentBasis, state: &ReducerState) -> Option<SessionKey> {
    matches!(
        pane.runtime.phase,
        SessionPhase::Starting { .. } | SessionPhase::Running { .. }
    )
    .then_some(SessionKey {
        pane: pane.pane,
        epoch: state.epoch(),
        generation: pane.runtime.session_generation,
    })
}

fn work_state(
    pane: &PaneAssessmentBasis,
    now: ReducerTick,
    state: &ReducerState,
) -> CloseWorkState {
    let Some(session) = live_session(pane, state) else {
        return CloseWorkState::Inactive;
    };
    if matches!(pane.runtime.phase, SessionPhase::Starting { .. }) {
        return CloseWorkState::Unknown;
    }
    pane.close_work
        .filter(|v| {
            v.session() == session
                && v.generation().get() != 0
                && v.is_fresh(now, state.limits.close_work_max_age_ms)
        })
        .map(|v| match v.state() {
            CloseWorkState::Inactive => CloseWorkState::Unknown,
            classification => classification,
        })
        .unwrap_or(CloseWorkState::Unknown)
}

fn busy(
    state: &ReducerState,
    record: &AssessmentRecord,
    reservation_owner: Option<OperationBinding>,
) -> bool {
    if matches!(record.target, AssessmentTarget::EndAllSessions(_)) {
        // Job C may overlap ordinary/Git retirement, but not a second server-wide batch.
        return state.creation_barrier();
    }
    state.creation_barrier()
        || state.runtime_cleanups.values().any(|owner| {
            record
                .workspaces
                .iter()
                .any(|w| w.workspace == owner.pane.workspace())
        })
        || state.retirements.values().any(|descriptor| {
            descriptor.panes().iter().any(|pane| {
                record
                    .workspaces
                    .iter()
                    .any(|w| w.workspace == pane.workspace())
            })
        })
        || state.git_allocations.values().any(|reservation| {
            Some(reservation.binding()) != reservation_owner
                && reservation.parts().fallbacks.iter().any(|fallback| {
                    record
                        .workspaces
                        .iter()
                        .any(|w| w.workspace == fallback.plan.workspace)
                })
        })
}

fn capture(
    state: &ReducerState,
    target: AssessmentTarget,
    reference: AssessmentRef,
    principal: MutationPrincipal,
    now: ReducerTick,
) -> Result<AssessmentRecord, ReducerFailure> {
    let org = state.organization();
    target
        .validate_scope(org.server)
        .map_err(|_| ReducerFailure::WrongServer)?;
    match target {
        AssessmentTarget::Close(CloseTarget::Pane(id)) if !org.panes.iter().any(|v| v.id == id) => {
            return Err(ReducerFailure::TargetNotFound);
        }
        AssessmentTarget::Close(CloseTarget::Tab(id)) if !org.tabs.iter().any(|v| v.id == id) => {
            return Err(ReducerFailure::TargetNotFound);
        }
        AssessmentTarget::Close(CloseTarget::Workspace(id))
        | AssessmentTarget::KillWorkspace(id)
            if !org.workspaces.iter().any(|v| v.id == id) =>
        {
            return Err(ReducerFailure::TargetNotFound);
        }
        AssessmentTarget::Close(CloseTarget::WorktreeTabs {
            workspace,
            worktree,
        }) => {
            if !org.workspaces.iter().any(|v| v.id == workspace)
                || !org.worktrees.iter().any(|v| v.id == worktree)
            {
                return Err(ReducerFailure::TargetNotFound);
            }
        }
        AssessmentTarget::RemoveWorktree { scope, worktree } => {
            let record = org
                .worktrees
                .iter()
                .find(|v| v.id == worktree)
                .ok_or(ReducerFailure::TargetNotFound)?;
            if record.repository != scope.repository {
                return Err(ReducerFailure::WrongMembership);
            }
        }
        _ => {}
    }
    let tabs: std::collections::BTreeMap<_, _> = org.tabs.iter().map(|v| (v.id, v)).collect();
    let runtime: std::collections::BTreeMap<_, _> = state
        .snapshot
        .runtime()
        .iter()
        .map(|v| (v.pane, *v))
        .collect();
    let mut workspace_tabs = std::collections::BTreeMap::<_, Vec<TabId>>::new();
    for tab in &org.tabs {
        workspace_tabs
            .entry(tab.workspace)
            .or_default()
            .push(tab.id);
    }
    let mut members = std::collections::BTreeMap::<_, Vec<PaneId>>::new();
    for pane in &org.panes {
        members.entry(pane.tab).or_default().push(pane.id);
    }
    let mut affected_tabs = BTreeSet::new();
    let mut affected_workspaces = BTreeSet::new();
    let mut panes = Vec::new();
    for pane in &org.panes {
        let tab = tabs.get(&pane.tab).ok_or(ReducerFailure::InvalidFacts)?;
        let matches = match target {
            AssessmentTarget::Close(CloseTarget::Pane(id)) => pane.id == id,
            AssessmentTarget::Close(CloseTarget::Tab(id)) => tab.id == id,
            AssessmentTarget::Close(CloseTarget::Workspace(id))
            | AssessmentTarget::KillWorkspace(id) => tab.workspace == id,
            AssessmentTarget::Close(CloseTarget::WorktreeTabs {
                workspace,
                worktree,
            }) => tab.workspace == workspace && tab.worktree == Some(worktree),
            AssessmentTarget::RemoveWorktree { worktree, .. } => tab.worktree == Some(worktree),
            AssessmentTarget::EndAllSessions(_) => true,
        };
        if matches {
            affected_tabs.insert(tab.id);
            affected_workspaces.insert(tab.workspace);
            panes.push(PaneAssessmentBasis {
                pane: pane.id,
                tab: pane.tab,
                runtime: *runtime.get(&pane.id).ok_or(ReducerFailure::InvalidFacts)?,
                close_work: state.observations.get(&pane.id).and_then(|v| v.close_work),
            });
        }
    }
    if panes.is_empty()
        && !matches!(
            target,
            AssessmentTarget::EndAllSessions(_) | AssessmentTarget::RemoveWorktree { .. }
        )
    {
        return Err(ReducerFailure::TargetNotFound);
    }
    let workspaces: Vec<_> = org
        .workspaces
        .iter()
        .filter(|v| affected_workspaces.contains(&v.id))
        .map(|v| {
            Ok(WorkspaceAssessmentBasis {
                workspace: v.id,
                root_tab: v.root_tab,
                tabs: workspace_tabs
                    .remove(&v.id)
                    .ok_or(ReducerFailure::InvalidFacts)?,
            })
        })
        .collect::<Result<_, ReducerFailure>>()?;
    // Sibling Tab records bind Worktree classes, Roots and succession order. Only
    // affected Tabs bind full Pane membership; sibling runtime is not close work.
    let tabs: Vec<_> = org
        .tabs
        .iter()
        .filter(|v| affected_workspaces.contains(&v.workspace))
        .cloned()
        .collect();
    let tab_panes: Vec<_> = affected_tabs
        .into_iter()
        .map(|tab| {
            Ok((
                tab,
                members.remove(&tab).ok_or(ReducerFailure::InvalidFacts)?,
            ))
        })
        .collect::<Result<_, ReducerFailure>>()?;
    let (retiring_panes, retiring_sessions) =
        if matches!(target, AssessmentTarget::EndAllSessions(_)) {
            super::lifecycle::retiring_basis(state)?
        } else {
            (Vec::new(), Vec::new())
        };
    // Charge the complete domain records and their vector values, including the
    // registry key. None of these engine-free facts contain terminal content.
    let mut bytes = std::mem::size_of::<AssessmentRecord>()
        .checked_add(std::mem::size_of::<OperationId>())
        .ok_or(ReducerFailure::QuotaExceeded)?;
    charge::<WorkspaceAssessmentBasis>(&mut bytes, workspaces.len())?;
    for workspace in &workspaces {
        charge::<TabId>(&mut bytes, workspace.tabs.len())?;
    }
    charge::<crate::organization::TabRecord>(&mut bytes, tabs.len())?;
    charge::<(TabId, Vec<PaneId>)>(&mut bytes, tab_panes.len())?;
    for (_, members) in &tab_panes {
        charge::<PaneId>(&mut bytes, members.len())?;
    }
    charge::<PaneAssessmentBasis>(&mut bytes, panes.len())?;
    charge::<crate::retirement::RetirementPane>(&mut bytes, retiring_panes.len())?;
    charge::<crate::retirement::RetiredSession>(&mut bytes, retiring_sessions.len())?;
    let requires_confirmation = matches!(
        target,
        AssessmentTarget::KillWorkspace(_) | AssessmentTarget::EndAllSessions(_)
    ) || panes.iter().any(|pane| {
        matches!(
            work_state(pane, now, state),
            CloseWorkState::RunningWork | CloseWorkState::Unknown
        )
    });
    Ok(AssessmentRecord {
        reference,
        principal,
        epoch: state.epoch(),
        target,
        captured_at: now,
        expires_at: now,
        workspaces,
        tabs,
        tab_panes,
        panes,
        retiring_panes,
        retiring_sessions,
        requires_confirmation,
        value_bytes: bytes,
    })
}

fn charge<T>(bytes: &mut usize, count: usize) -> Result<(), ReducerFailure> {
    *bytes = count
        .checked_mul(std::mem::size_of::<T>())
        .and_then(|n| bytes.checked_add(n))
        .ok_or(ReducerFailure::QuotaExceeded)?;
    Ok(())
}

fn summary(record: &AssessmentRecord) -> Result<AssessmentSummary, ReducerFailure> {
    let count = |value: usize| u32::try_from(value).map_err(|_| ReducerFailure::CounterExhausted);
    let workspaces: BTreeSet<_> = record
        .workspaces
        .iter()
        .map(|v| v.workspace)
        .chain(record.retiring_panes.iter().map(|v| v.workspace()))
        .collect();
    let tabs: BTreeSet<_> = record
        .panes
        .iter()
        .map(|v| v.tab)
        .chain(record.retiring_panes.iter().map(|v| v.tab()))
        .collect();
    let panes: BTreeSet<_> = record
        .panes
        .iter()
        .map(|v| v.pane)
        .chain(record.retiring_panes.iter().map(|v| v.pane()))
        .collect();
    let live: BTreeSet<_> = record
        .panes
        .iter()
        .filter(|v| {
            matches!(
                v.runtime.phase,
                SessionPhase::Starting { .. } | SessionPhase::Running { .. }
            )
        })
        .map(|v| v.pane)
        .chain(record.retiring_sessions.iter().map(|v| v.session().pane))
        .collect();
    AssessmentSummary::new(
        record.target,
        count(workspaces.len())?,
        count(tabs.len())?,
        count(panes.len())?,
        count(live.len())?,
        record.requires_confirmation,
    )
    .map_err(|_| ReducerFailure::InvalidFacts)
}

#[cfg(test)]
mod tests {
    use super::AssessmentFailure;
    use crate::operation_ledger_contracts::*;
    use crate::retirement::RetirementCleanupState;

    #[expect(
        unused_imports,
        reason = "The external fixture's storage reexports overlap crate-visible reducer imports here."
    )]
    mod support {
        use crate as spaceterm_core;
        include!("../../tests/organization_reducer/support.rs");
    }
    use support::*;

    // Lifecycle is Job C's port. This in-crate fake supplies only its frozen
    // runtime and observation facts; every assessment/deletion assertion enters
    // the public reducer API. It supplies no launch or process-cleanup evidence.
    fn owner_facts(
        reducer: &mut OrganizationReducer,
        pane: PaneId,
        generation: u64,
        starting: bool,
        work: Option<(u64, u64, u64, CloseWorkState)>,
    ) -> SessionKey {
        let generation = SessionGeneration::new(generation);
        let session = SessionKey {
            pane,
            epoch: reducer.snapshot().epoch(),
            generation,
        };
        let mut org = reducer.snapshot().state().clone();
        let mut facts = org.retained.as_ref().unwrap().parts().clone();
        let mark = facts
            .pane_generations
            .iter_mut()
            .find(|v| v.pane() == pane)
            .unwrap();
        *mark = PaneGenerationHighWater::new(pane, generation, mark.control());
        org.retained = Some(OrganizationRetainedFacts::new(facts).unwrap());
        let mut runtime = reducer.snapshot().runtime().to_vec();
        let record = runtime.iter_mut().find(|v| v.pane == pane).unwrap();
        record.session_generation = generation;
        record.phase = if starting {
            SessionPhase::Starting { generation }
        } else {
            SessionPhase::Running { generation }
        };
        reducer.state.replace_organization(org, runtime).unwrap();
        reducer
            .state
            .observations
            .entry(pane)
            .or_default()
            .close_work = work.map(|(sequence, generation, tick, state)| {
            CloseWorkObservation::new(
                session,
                RuntimeObservationSequence::new(sequence).unwrap(),
                CloseWorkGeneration::new(generation),
                ReducerTick::new(tick),
                state,
            )
            .unwrap()
        });
        reducer.state.transition += 1;
        session
    }

    fn assess_at(
        reducer: &mut OrganizationReducer,
        target: AssessmentTarget,
        sequence: u64,
        now: u64,
    ) -> CloseAssessment {
        let command = match target {
            AssessmentTarget::Close(target) => Command::AssessClose { target },
            AssessmentTarget::KillWorkspace(workspace) => {
                Command::AssessKillWorkspace { workspace }
            }
            AssessmentTarget::EndAllSessions(server) => Command::AssessEndAllSessions { server },
            _ => panic!("external assessment needs Git policy"),
        };
        let mut context = context(sequence);
        context.now = ReducerTick::new(now);
        let prepared = reducer.prepare_command(&command, &context).unwrap();
        let committed = commit(reducer, prepared);
        match committed.resolution {
            Resolution::Completed(Outcome::Assessment(value)) => *value,
            _ => panic!("assessment did not complete"),
        }
    }
    fn valid(
        reducer: &OrganizationReducer,
        value: CloseAssessment,
        now: u64,
    ) -> Result<(), AssessmentFailure> {
        reducer.validate_assessment(
            &context(1).principal,
            &value.reference(),
            &value.summary().target(),
            ReducerTick::new(now),
        )
    }

    /// The ledger implementation is another Wave 2 track. This fake implements
    /// only the frozen codec port used to prepare a compact Applied result.
    struct OutcomeCodec;
    impl LedgerCodec for OutcomeCodec {
        fn canonical_request_digest(
            &self,
            _: &StreamBinding,
            _: OperationId,
            _: &Command,
        ) -> Result<RequestDigest, LedgerFailure> {
            Err(LedgerFailure::new(LedgerFailureClass::InvalidRequest))
        }
        fn encode_canonical_request(
            &self,
            _: &StreamBinding,
            _: OperationId,
            _: &Command,
        ) -> Result<Vec<u8>, LedgerFailure> {
            Err(LedgerFailure::new(LedgerFailureClass::InvalidRequest))
        }
        fn decode_canonical_request(
            &self,
            _: &[u8],
        ) -> Result<CanonicalOperationRequest, LedgerFailure> {
            Err(LedgerFailure::new(LedgerFailureClass::InvalidRequest))
        }
        fn encode_compact_outcome(&self, outcome: &Outcome) -> Result<Vec<u8>, LedgerFailure> {
            if *outcome == Outcome::Applied {
                Ok(vec![0xa5])
            } else {
                Err(LedgerFailure::new(
                    LedgerFailureClass::InvalidCompactOutcome,
                ))
            }
        }
        fn decode_compact_outcome(&self, bytes: &[u8]) -> Result<Outcome, LedgerFailure> {
            if bytes == [0xa5] {
                Ok(Outcome::Applied)
            } else {
                Err(LedgerFailure::new(
                    LedgerFailureClass::InvalidCompactOutcome,
                ))
            }
        }
    }
    fn closing_context(sequence: u64) -> ReductionContext {
        let mut context = context(sequence);
        context.facts.retirement_outcome =
            Some(AppliedRetirementOutcome::new(&OutcomeCodec).unwrap());
        context
    }

    #[test]
    fn live_close_classifies_only_fresh_checked_idle_facts_as_safe() {
        for (starting, work, requires) in [
            (false, None, true),
            (false, Some((1, 1, 10, CloseWorkState::Idle)), false),
            (false, Some((1, 1, 0, CloseWorkState::Idle)), true),
            (false, Some((1, 1, 5_012, CloseWorkState::Idle)), true),
            (false, Some((1, 1, 10, CloseWorkState::RunningWork)), true),
            (false, Some((1, 1, 10, CloseWorkState::Unknown)), true),
            (false, Some((1, 1, 10, CloseWorkState::Inactive)), true),
            (false, Some((1, 0, 10, CloseWorkState::Idle)), true),
            (true, Some((1, 1, 10, CloseWorkState::Idle)), true),
        ] {
            let mut reducer = reducer();
            let (_, _, pane) = create(&mut reducer, 1);
            owner_facts(&mut reducer, pane, 1, starting, work);
            let value = assess_at(
                &mut reducer,
                AssessmentTarget::Close(CloseTarget::Pane(pane)),
                2,
                5_010,
            );
            assert_eq!(value.summary().requires_confirmation(), requires);
            assert_eq!(value.summary().live_session_count(), 1);
        }
    }

    #[test]
    fn session_replacement_and_new_work_generation_invalidate_assessment() {
        for replacement_session in [false, true] {
            let mut reducer = reducer();
            let (_, _, pane) = create(&mut reducer, 1);
            owner_facts(
                &mut reducer,
                pane,
                1,
                false,
                Some((1, 1, 10, CloseWorkState::Idle)),
            );
            let value = assess_at(
                &mut reducer,
                AssessmentTarget::Close(CloseTarget::Pane(pane)),
                2,
                10,
            );
            owner_facts(
                &mut reducer,
                pane,
                if replacement_session { 2 } else { 1 },
                false,
                Some((2, 2, 11, CloseWorkState::RunningWork)),
            );
            assert_eq!(
                valid(&reducer, value, 11),
                Err(AssessmentFailure::StaleAssessment)
            );
            let before = reducer.snapshot();
            let prepared = reducer
                .prepare_command(
                    &Command::CloseConfirmed {
                        target: CloseTarget::Pane(pane),
                        assessment: value.reference(),
                    },
                    &closing_context(3),
                )
                .unwrap();
            let result = commit(&mut reducer, prepared);
            assert_eq!(
                result.resolution,
                Resolution::Completed(Outcome::Failed(OperationFailure::StaleAssessment))
            );
            assert!(result.effects.is_empty() && result.publication.is_none());
            assert_eq!(reducer.snapshot(), before);
        }
    }

    #[test]
    fn idle_assessment_expires_as_work_fact_ages_but_same_generation_heartbeat_is_allowed() {
        let mut reducer = reducer();
        let (_, _, pane) = create(&mut reducer, 1);
        owner_facts(
            &mut reducer,
            pane,
            1,
            false,
            Some((1, 1, 10, CloseWorkState::Idle)),
        );
        let value = assess_at(
            &mut reducer,
            AssessmentTarget::Close(CloseTarget::Pane(pane)),
            2,
            10,
        );
        assert_eq!(valid(&reducer, value, 5_010), Ok(()));
        assert_eq!(
            valid(&reducer, value, 5_011),
            Err(AssessmentFailure::StaleAssessment)
        );
        owner_facts(
            &mut reducer,
            pane,
            1,
            false,
            Some((2, 1, 50, CloseWorkState::Idle)),
        );
        assert_eq!(valid(&reducer, value, 5_011), Ok(()));
        assert_eq!(
            valid(&reducer, value, 60_010),
            Err(AssessmentFailure::AssessmentExpired)
        );
    }

    #[test]
    fn close_another_clients_tab_requires_confirmation_but_not_takeover() {
        let mut reducer = reducer();
        let (_, tab, pane) = create(&mut reducer, 1);
        let session = owner_facts(
            &mut reducer,
            pane,
            1,
            false,
            Some((1, 1, 10, CloseWorkState::RunningWork)),
        );
        let target = CloseTarget::Tab(tab);
        let mut caller = closing_context(2);
        caller.principal.client = ClientId::new(SERVER, 2).unwrap();
        caller.operation = OperationId::new(
            OperationStreamId::new(
                OperationStreamKey::new(SERVER, 2).unwrap(),
                caller.principal.pairing,
                caller.principal.client,
            )
            .unwrap(),
            OperationSequence::new(2).unwrap(),
        );
        let prepared = reducer
            .prepare_command(&Command::AssessClose { target }, &caller)
            .unwrap();
        let value = match commit(&mut reducer, prepared).resolution {
            Resolution::Completed(Outcome::Assessment(value)) => *value,
            _ => panic!("assessment did not complete"),
        };
        assert!(value.summary().requires_confirmation());
        caller.operation = OperationId::new(
            caller.operation.stream(),
            OperationSequence::new(3).unwrap(),
        );
        let prepared = reducer
            .prepare_command(
                &Command::CloseConfirmed {
                    target,
                    assessment: value.reference(),
                },
                &caller,
            )
            .unwrap();
        let before = reducer.snapshot();
        assert_eq!(
            before.runtime()[0].control,
            ControlRecord::Held {
                client: ClientId::new(SERVER, 1).unwrap(),
                generation: ControlGeneration::new(1)
            }
        );
        let result = commit(&mut reducer, prepared);
        let descriptor = match result.resolution {
            Resolution::AwaitingRetirement(plan) => plan.descriptor,
            _ => panic!("live close completed before cleanup"),
        };
        assert_eq!(descriptor.binding(), caller.binding());
        assert_eq!(descriptor.sessions()[0].session(), session);
        assert!(!descriptor.cleanup_confirmed());
        assert!(reducer.snapshot().state().workspaces.is_empty());
    }

    #[test]
    fn accepted_live_close_deletes_once_and_retains_exact_pending_cleanup() {
        for kill in [false, true] {
            let mut reducer = reducer();
            let (workspace, tab, pane) = create(&mut reducer, 1);
            let session = owner_facts(
                &mut reducer,
                pane,
                7,
                false,
                Some((1, 2, 10, CloseWorkState::RunningWork)),
            );
            let target = if kill {
                AssessmentTarget::KillWorkspace(workspace)
            } else {
                AssessmentTarget::Close(CloseTarget::Tab(tab))
            };
            let value = assess_at(&mut reducer, target, 2, 10);
            let command = if kill {
                Command::KillWorkspaceConfirmed {
                    workspace,
                    assessment: value.reference(),
                }
            } else {
                Command::CloseConfirmed {
                    target: CloseTarget::Tab(tab),
                    assessment: value.reference(),
                }
            };
            let before = reducer.snapshot();
            let prepared = reducer
                .prepare_command(&command, &closing_context(3))
                .unwrap();
            let descriptor = match prepared.resolution() {
                Resolution::AwaitingRetirement(plan) => plan.descriptor.clone(),
                _ => panic!("live close completed before cleanup"),
            };
            assert_eq!(reducer.snapshot(), before);
            assert!(reducer.retained().pending_retirements().is_empty());
            assert_eq!(
                descriptor.acceptance_revision(),
                prepared.resulting_organization_revision()
            );
            assert_eq!(descriptor.assessment(), value.reference());
            assert_eq!(
                descriptor.panes()[0].invalidation_generation(),
                SessionGeneration::new(8)
            );
            assert_eq!(descriptor.sessions()[0].session(), session);
            assert_eq!(
                descriptor.sessions()[0].cleanup(),
                RetirementCleanupState::AwaitingEvidence
            );
            assert_eq!(
                descriptor
                    .parts()
                    .eventual_result
                    .outcome(&OutcomeCodec)
                    .unwrap(),
                Outcome::Applied
            );
            let result = commit(&mut reducer, prepared);
            assert_eq!(
                replay(before, result.publication.as_ref().unwrap()),
                reducer.snapshot()
            );
            assert_eq!(
                reducer.retained().pending_retirements(),
                std::slice::from_ref(&descriptor)
            );
            assert_eq!(
                result
                    .effects
                    .iter()
                    .filter(|v| matches!(v, OrganizationEffect::RetireSession { .. }))
                    .count(),
                1
            );
            assert!(
                matches!(result.effects.last(), Some(OrganizationEffect::RetireSession { session: key, reason, .. }) if *key == session && *reason == descriptor.reason())
            );
            let rejected = apply(&mut reducer, command, 4);
            assert_eq!(
                rejected.resolution,
                Resolution::Completed(Outcome::Failed(OperationFailure::TargetDeleted))
            );
            assert!(rejected.publication.is_none() && rejected.effects.is_empty());
            let restored = OrganizationReducer::from_recovered(
                reducer.retained(),
                reducer.store_revision(),
                ServerEpoch::new(10),
                OrganizationLimits::default(),
            )
            .unwrap();
            assert!(restored.snapshot().state().panes.is_empty());
            assert_eq!(restored.retained().pending_retirements(), &[descriptor]);
            assert!(!restored.retained().pending_retirements()[0].cleanup_confirmed());
        }
    }

    #[test]
    fn live_close_without_real_codec_input_fails_closed_before_deletion() {
        let mut reducer = reducer();
        let (_, _, pane) = create(&mut reducer, 1);
        owner_facts(&mut reducer, pane, 1, false, None);
        let value = assess_at(
            &mut reducer,
            AssessmentTarget::Close(CloseTarget::Pane(pane)),
            2,
            10,
        );
        let before = reducer.snapshot();
        assert!(matches!(
            reducer.prepare_command(
                &Command::CloseConfirmed {
                    target: CloseTarget::Pane(pane),
                    assessment: value.reference()
                },
                &context(3)
            ),
            Err(ReducerFailure::InvalidFacts)
        ));
        assert_eq!(reducer.snapshot(), before);
        assert_eq!(valid(&reducer, value, 10), Ok(()));
    }

    #[test]
    fn pending_deletion_gates_surviving_workspace_but_not_unrelated_work() {
        let mut reducer = OrganizationReducer::from_recovered(
            with_worktrees(empty()),
            StoreRevision::new(0),
            ServerEpoch::new(9),
            OrganizationLimits::default(),
        )
        .unwrap();
        let (workspace, tab, pane) = create(&mut reducer, 1);
        let (other_workspace, _, other) = create(&mut reducer, 2);
        apply(
            &mut reducer,
            Command::CreatePane {
                tab,
                source: pane,
                axis: SplitAxis::Horizontal,
                edge: SplitEdge::After,
            },
            3,
        );
        let survivor = reducer
            .snapshot()
            .state()
            .panes
            .iter()
            .find(|v| v.tab == tab && v.id != pane)
            .unwrap()
            .id;
        owner_facts(&mut reducer, pane, 1, false, None);
        let sibling = assess_at(
            &mut reducer,
            AssessmentTarget::Close(CloseTarget::Pane(survivor)),
            4,
            10,
        );
        let value = assess_at(
            &mut reducer,
            AssessmentTarget::Close(CloseTarget::Pane(pane)),
            5,
            10,
        );
        let prepared = reducer
            .prepare_command(
                &Command::CloseConfirmed {
                    target: CloseTarget::Pane(pane),
                    assessment: value.reference(),
                },
                &closing_context(6),
            )
            .unwrap();
        commit(&mut reducer, prepared);
        assert_eq!(
            valid(&reducer, sibling, 10),
            Err(AssessmentFailure::StaleAssessment)
        );
        let result = apply(&mut reducer, Command::AssessKillWorkspace { workspace }, 7);
        assert_eq!(
            result.resolution,
            Resolution::Completed(Outcome::Failed(OperationFailure::BusyRetiring))
        );
        let result = apply(
            &mut reducer,
            Command::AssessClose {
                target: CloseTarget::Pane(other),
            },
            8,
        );
        assert!(matches!(
            result.resolution,
            Resolution::Completed(Outcome::Assessment(_))
        ));
        assert!(
            reducer
                .snapshot()
                .state()
                .workspaces
                .iter()
                .any(|v| v.id == other_workspace)
        );
    }

    #[test]
    fn retirement_count_and_byte_bounds_reject_before_cascade_or_batch_consumption() {
        for dimension in 0..2 {
            let mut reducer = reducer();
            let (_, _, pane) = create(&mut reducer, 1);
            owner_facts(&mut reducer, pane, 1, false, None);
            if dimension == 0 {
                reducer.state.limits.retirement_batches = 0;
            } else {
                reducer.state.limits.retirement_bytes = 496;
            } // One byte below the frozen descriptor charge.
            let value = assess_at(
                &mut reducer,
                AssessmentTarget::Close(CloseTarget::Pane(pane)),
                2,
                10,
            );
            let before = reducer.snapshot();
            let batch = reducer.next_retirement_batch().unwrap();
            let prepared = reducer
                .prepare_command(
                    &Command::CloseConfirmed {
                        target: CloseTarget::Pane(pane),
                        assessment: value.reference(),
                    },
                    &closing_context(3),
                )
                .unwrap();
            let result = commit(&mut reducer, prepared);
            assert_eq!(
                result.resolution,
                Resolution::Completed(Outcome::Failed(OperationFailure::Capacity))
            );
            assert!(result.publication.is_none() && result.effects.is_empty());
            assert_eq!(reducer.snapshot(), before);
            assert_eq!(reducer.next_retirement_batch().unwrap(), batch);
            assert_eq!(valid(&reducer, value, 10), Ok(()));
        }
    }

    #[test]
    fn retirement_capacity_is_aggregate_across_deleted_workspaces() {
        for byte_limit in [false, true] {
            let mut reducer = reducer();
            let mut targets = Vec::new();
            for sequence in 1..=3 {
                let (_, _, pane) = create(&mut reducer, sequence);
                owner_facts(&mut reducer, pane, 1, false, None);
                targets.push(pane);
            }
            if byte_limit {
                reducer.state.limits.retirement_bytes = 994;
            } else {
                reducer.state.limits.retirement_batches = 2;
            }
            for (index, pane) in targets.into_iter().enumerate() {
                let sequence = 4 + index as u64 * 2;
                let value = assess_at(
                    &mut reducer,
                    AssessmentTarget::Close(CloseTarget::Pane(pane)),
                    sequence,
                    10,
                );
                let before = reducer.snapshot();
                let prepared = reducer
                    .prepare_command(
                        &Command::CloseConfirmed {
                            target: CloseTarget::Pane(pane),
                            assessment: value.reference(),
                        },
                        &closing_context(sequence + 1),
                    )
                    .unwrap();
                let result = commit(&mut reducer, prepared);
                if index < 2 {
                    assert!(matches!(
                        result.resolution,
                        Resolution::AwaitingRetirement(_)
                    ));
                    assert_eq!(reducer.retained().pending_retirements().len(), index + 1);
                } else {
                    assert_eq!(
                        result.resolution,
                        Resolution::Completed(Outcome::Failed(OperationFailure::Capacity))
                    );
                    assert!(result.publication.is_none() && result.effects.is_empty());
                    assert_eq!(reducer.snapshot(), before);
                    assert_eq!(valid(&reducer, value, 10), Ok(()));
                }
            }
        }
    }

    #[test]
    fn retirement_session_ceiling_accepts_64_and_rejects_65_without_partial_deletion() {
        for count in [64, 65] {
            let mut reducer = reducer();
            let (_, tab, first) = create(&mut reducer, 1);
            for sequence in 2..=count {
                apply(
                    &mut reducer,
                    Command::CreatePane {
                        tab,
                        source: first,
                        axis: SplitAxis::Horizontal,
                        edge: SplitEdge::After,
                    },
                    sequence,
                );
            }
            let panes: Vec<_> = reducer
                .snapshot()
                .state()
                .panes
                .iter()
                .map(|v| v.id)
                .collect();
            for pane in panes {
                owner_facts(&mut reducer, pane, 1, false, None);
            }
            let value = assess_at(
                &mut reducer,
                AssessmentTarget::Close(CloseTarget::Tab(tab)),
                count + 1,
                10,
            );
            assert_eq!(u64::from(value.summary().live_session_count()), count);
            let before = reducer.snapshot();
            let prepared = reducer
                .prepare_command(
                    &Command::CloseConfirmed {
                        target: CloseTarget::Tab(tab),
                        assessment: value.reference(),
                    },
                    &closing_context(count + 2),
                )
                .unwrap();
            let result = commit(&mut reducer, prepared);
            if count == 64 {
                let descriptor = match result.resolution {
                    Resolution::AwaitingRetirement(plan) => plan.descriptor,
                    _ => panic!("bounded retirement did not remain pending"),
                };
                assert_eq!(descriptor.sessions().len(), 64);
                assert!(reducer.snapshot().state().panes.is_empty());
                assert_eq!(
                    result
                        .effects
                        .iter()
                        .filter(|v| matches!(v, OrganizationEffect::RetireSession { .. }))
                        .count(),
                    64
                );
            } else {
                assert_eq!(
                    result.resolution,
                    Resolution::Completed(Outcome::Failed(OperationFailure::Capacity))
                );
                assert!(result.publication.is_none() && result.effects.is_empty());
                assert_eq!(reducer.snapshot(), before);
                assert!(reducer.retained().pending_retirements().is_empty());
                assert_eq!(valid(&reducer, value, 10), Ok(()));
            }
        }
    }
}
