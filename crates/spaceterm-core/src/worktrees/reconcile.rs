//! Pure inventory and durable-intent validation. No reconciliation operation executes Git.
use super::*;
use crate::identity::{IdentityHighWater, RepositoryId, ServerId, WorktreeId};
use crate::organization::{OrganizationSnapshot, OrganizationWrite, ServerDirectory};
use crate::storage::StoreFailure;
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedInventoryEntry {
    pub retained: Option<WorktreeId>,
    pub directory: ServerDirectory,
    pub main: bool,
    pub missing: bool,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedWorktreeInventory {
    pub repository: RepositoryId,
    pub records: Vec<ValidatedInventoryEntry>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorktreeReconciliation {
    pub writes: Vec<OrganizationWrite>,
    /// The actor allocates fresh identities for these entries. The policy never invents IDs.
    pub additions: Vec<ValidatedInventoryEntry>,
}

pub fn plan_inventory_reconciliation(
    organization: &OrganizationSnapshot,
    inventory: &ValidatedWorktreeInventory,
) -> Result<WorktreeReconciliation, GitFailure> {
    use crate::organization::WorktreeRecord;
    use std::collections::{BTreeMap, BTreeSet};
    let state = organization.state();
    if inventory.repository.server() != state.server {
        return Err(GitFailure::WrongServer);
    }
    if !state
        .repositories
        .iter()
        .any(|r| r.id == inventory.repository)
    {
        return Err(GitFailure::TargetGone);
    }
    if inventory.records.len() > MAX_GIT_REPOSITORIES {
        return Err(GitFailure::Capacity);
    }
    let retained: BTreeMap<_, _> = state
        .worktrees
        .iter()
        .filter(|w| w.repository == inventory.repository)
        .map(|w| (w.id, w))
        .collect();
    let mut seen = BTreeSet::new();
    let mut paths = BTreeSet::new();
    let mut writes = Vec::new();
    let mut additions = Vec::new();
    let mut main = false;
    for entry in &inventory.records {
        if entry.directory.server() != state.server {
            return Err(GitFailure::WrongServer);
        }
        if !paths.insert(entry.directory.bytes()) || (entry.main && main) {
            return Err(GitFailure::InvalidResponse);
        }
        main |= entry.main;
        match entry.retained {
            Some(id) => {
                if !seen.insert(id) {
                    return Err(GitFailure::InvalidResponse);
                }
                let old = retained.get(&id).ok_or(GitFailure::IdentityReplaced)?;
                // A provider maps physical aliases to the retained spelling before proposing edits.
                // Replacement at the same path is a new inventory entry, never a reassigned ID.
                if old.directory != entry.directory || old.main != entry.main {
                    return Err(GitFailure::IdentityReplaced);
                }
                if old.missing != entry.missing {
                    writes.push(OrganizationWrite::PutWorktree(WorktreeRecord {
                        missing: entry.missing,
                        ..(*old).clone()
                    }));
                }
            }
            None => {
                if retained.values().any(|w| w.directory == entry.directory) {
                    return Err(GitFailure::IdentityReplaced);
                }
                additions.push(entry.clone());
            }
        }
    }
    // A failed read never calls this planner. Unlisted entries stay retained, including IDs
    // referenced by Tabs, and cannot be reused for a new object at their old location.
    for old in retained
        .values()
        .filter(|w| !seen.contains(&w.id) && !w.missing)
    {
        writes.push(OrganizationWrite::PutWorktree(WorktreeRecord {
            missing: true,
            ..(*old).clone()
        }));
    }
    Ok(WorktreeReconciliation { writes, additions })
}

/// Cross-check all auxiliary producers against the borrowed ledger and allocation marks.
/// This validates comparison facts only; recovery obtains fresh host authority separately.
pub fn validate_git_recovery(
    server: ServerId,
    marks: IdentityHighWater,
    repositories: &[RepositoryLocator],
    intents: &[GitWriteIntent],
    allocations: &[GitAllocationReservation],
    ledger: &dyn GitRecoveryLedger,
) -> Result<(), StoreFailure> {
    use std::collections::BTreeMap;
    let invalid = StoreFailure::InvalidDurableState;
    if repositories.len() > MAX_GIT_REPOSITORIES
        || intents.len() > MAX_GIT_INTENTS
        || allocations.len() > MAX_GIT_INTENTS
    {
        return Err(invalid);
    }
    let mut locators = BTreeMap::new();
    for locator in repositories {
        if locator.scope().server != server
            || locator.scope().repository.local() > marks.repository
            || locators
                .insert(locator.scope().repository, locator)
                .is_some()
        {
            return Err(invalid);
        }
    }
    let mut reservations = BTreeMap::new();
    for allocation in allocations {
        validate_git_binding(allocation.binding(), server, marks)?;
        if !marks.dominates(allocation.parts().resulting_high_water)
            || reservations
                .insert(allocation.binding().id, allocation)
                .is_some()
            || ledger.has_completed_git_result(allocation.binding().id)?
        {
            return Err(invalid);
        }
    }
    let mut operations = BTreeMap::new();
    let mut reserved_repositories = std::collections::BTreeSet::new();
    let mut reserved_worktrees = std::collections::BTreeSet::new();
    let mut reserved_tabs = std::collections::BTreeSet::new();
    let mut reserved_panes = std::collections::BTreeSet::new();
    let mut reserved_ordinals = std::collections::BTreeSet::new();
    for allocation in allocations {
        if allocation
            .parts()
            .repository
            .is_some_and(|r| !reserved_repositories.insert(r))
        {
            return Err(invalid);
        }
        if allocation
            .parts()
            .worktree
            .is_some_and(|w| !reserved_worktrees.insert(w))
        {
            return Err(invalid);
        }
        for fallback in &allocation.parts().fallbacks {
            if !reserved_tabs.insert(fallback.tab)
                || !reserved_panes.insert(fallback.pane)
                || !reserved_ordinals.insert(fallback.tab_creation_ordinal)
                || !reserved_ordinals.insert(fallback.pane_creation_ordinal)
            {
                return Err(invalid);
            }
        }
    }
    for intent in intents {
        validate_git_binding(intent.binding(), server, marks)?;
        if !intent.fits_marks(marks)
            || operations.insert(intent.binding().id, intent).is_some()
            || ledger.has_completed_git_result(intent.binding().id)?
            || ledger.is_awaiting_git_intent(intent.binding())?
            || ledger.pending_git_progress(intent.binding())? != Some(intent.reference())
        {
            return Err(invalid);
        }
        if locators
            .get(&intent.parts().repository.scope().repository)
            .copied()
            != Some(&intent.parts().repository)
        {
            return Err(invalid);
        }
        let allocation = reservations.get(&intent.binding().id).ok_or(invalid)?;
        if allocation.binding() != intent.binding() {
            return Err(invalid);
        }
        let plans: Vec<_> = allocation
            .parts()
            .fallbacks
            .iter()
            .map(|f| &f.plan)
            .collect();
        let expected: Vec<_> = intent.parts().affected.parts().fallbacks.iter().collect();
        if plans.len() != expected.len() || expected.iter().any(|p| !plans.contains(p)) {
            return Err(invalid);
        }
        match &intent.parts().action {
            RetainedGitAction::Create { worktree, .. }
                if allocation.parts().worktree != Some(*worktree) =>
            {
                return Err(invalid);
            }
            RetainedGitAction::Remove { .. }
                if allocation.parts().worktree.is_some()
                    || allocation.parts().repository.is_some() =>
            {
                return Err(invalid);
            }
            _ => {}
        }
        if allocation
            .parts()
            .repository
            .is_some_and(|r| r != intent.parts().repository.scope().repository)
        {
            return Err(invalid);
        }
        if intent.parts().external_result.as_ref().is_some_and(|r| {
            r.ended_sessions as usize > intent.parts().affected.parts().panes.len()
        }) {
            return Err(invalid);
        }
    }
    for allocation in allocations {
        if !operations.contains_key(&allocation.binding().id)
            && !ledger.is_awaiting_git_intent(allocation.binding())?
        {
            return Err(invalid);
        }
    }
    Ok(())
}

/// Apply the typed compare-and-swap without changing the request or original scope.
pub fn apply_intent_mutation(
    current: Option<&GitWriteIntent>,
    mutation: &GitIntentMutation,
) -> Result<Option<GitWriteIntent>, GitFailure> {
    mutation.validate()?;
    match (current, mutation) {
        (None, GitIntentMutation::Insert(intent)) => Ok(Some((**intent).clone())),
        (
            Some(current),
            GitIntentMutation::Advance {
                binding,
                expected_phase,
                next,
                result,
            },
        ) if current.binding() == *binding && current.phase() == *expected_phase => {
            let mut parts = current.parts().clone();
            parts.phase = *next;
            parts.external_result = result.clone();
            GitWriteIntent::new(parts).map(Some)
        }
        (Some(current), GitIntentMutation::Complete { binding })
            if current.binding() == *binding =>
        {
            Ok(None)
        }
        _ => Err(GitFailure::StaleAssessment),
    }
}
