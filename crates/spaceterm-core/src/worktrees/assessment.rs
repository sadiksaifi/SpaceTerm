//! Exact all-Workspace removal policy. Filesystem observations are explicit inputs.

use super::*;
use crate::authority::{
    CloseWorkGeneration, ContextGeneration, FieldVersion, ServerEpoch, SessionKey,
};
use crate::identity::{PaneId, WorkspaceId};
use crate::observations::ConfirmedDirectoryObservation;
use crate::operations::AssessedPane;
use crate::organization::{OrganizationSnapshot, ServerDirectory, SessionPhase, WorktreeRecord};
use crate::server_directories::DirectoryContainment;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeWorkStamp {
    pub pane: PaneId,
    pub session: Option<SessionKey>,
    pub context_generation: ContextGeneration,
    pub work_generation: CloseWorkGeneration,
    pub startup_reserved: bool,
    pub confirmed_directory: Option<ConfirmedDirectoryObservation>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeWorkSet {
    pub panes: Vec<RuntimeWorkStamp>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorktreeRemovalFacts {
    pub target: WorktreeRecord,
    pub listed: bool,
    pub locked: bool,
    pub head: Option<RetainedGitHead>,
    pub changes: bool,
    pub submodules: bool,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PinDirectoryContainment {
    pub workspace: WorkspaceId,
    pub directory: ServerDirectory,
    pub version: FieldVersion,
    pub containment: DirectoryContainment,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PaneDirectoryContainment {
    pub pane: PaneId,
    pub directory: ServerDirectory,
    pub containment: DirectoryContainment,
}
/// Entries are in Git order. A bare Main is ineligible; the first surviving linked entry wins.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FallbackWorktree {
    pub record: WorktreeRecord,
    pub bare: bool,
    pub available: bool,
    pub containment: DirectoryContainment,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AffectedWorktreeWork {
    pub pinned: Vec<PinDirectoryContainment>,
    pub panes: Vec<PaneDirectoryContainment>,
    pub fallbacks: Vec<FallbackWorktree>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorktreeRemovalScope {
    epoch: ServerEpoch,
    facts: WorktreeRemovalFacts,
    retained: RetainedRemovalScope,
    directories: Vec<PaneDirectoryContainment>,
}
impl WorktreeRemovalScope {
    pub fn retained(&self) -> &RetainedRemovalScope {
        &self.retained
    }
    pub fn facts(&self) -> &WorktreeRemovalFacts {
        &self.facts
    }
    pub fn approval(&self) -> ApprovedRemoval {
        match (
            self.facts.target.missing,
            self.facts.changes,
            self.facts.submodules,
        ) {
            (true, _, _) => ApprovedRemoval::Missing,
            (false, false, false) => ApprovedRemoval::Clean,
            (false, true, false) => ApprovedRemoval::DiscardChanges,
            (false, false, true) => ApprovedRemoval::DeleteSubmoduleCheckouts,
            (false, true, true) => ApprovedRemoval::DiscardChangesAndDeleteSubmoduleCheckouts,
        }
    }
}
/// Assess every retained Workspace, never a client's attachment list.
pub fn assess_removal(
    organization: &OrganizationSnapshot,
    runtime: &RuntimeWorkSet,
    target: &WorktreeRemovalFacts,
    affected: &AffectedWorktreeWork,
) -> Result<WorktreeRemovalScope, GitFailure> {
    let state = organization.state();
    if target.target.id.server() != state.server {
        return Err(GitFailure::WrongServer);
    }
    let current = state
        .worktrees
        .iter()
        .find(|w| w.id == target.target.id)
        .ok_or(GitFailure::TargetGone)?;
    if current != &target.target {
        return Err(GitFailure::GitStateChanged);
    }
    if current.main {
        return Err(GitFailure::MainWorktree);
    }
    if !target.listed {
        return Err(GitFailure::UnlistedWorktree);
    }
    if target.locked {
        return Err(GitFailure::LockedWorktree);
    }
    if current.missing && (target.changes || target.submodules) {
        return Err(GitFailure::GitStateChanged);
    }
    let pins = unique_map(&affected.pinned, |p| p.workspace)?;
    if pins.len()
        != state
            .workspaces
            .iter()
            .filter(|w| w.pinned_directory.value.is_some())
            .count()
    {
        return Err(GitFailure::Unchecked);
    }
    for workspace in &state.workspaces {
        if let Some(directory) = &workspace.pinned_directory.value {
            let pin = pins.get(&workspace.id).ok_or(GitFailure::Unchecked)?;
            if &pin.directory != directory || pin.version != workspace.pinned_directory.version {
                return Err(GitFailure::Unchecked);
            }
            match pin.containment {
                DirectoryContainment::Inside => return Err(GitFailure::HoldsPinnedDirectory),
                DirectoryContainment::Unchecked => return Err(GitFailure::Unchecked),
                DirectoryContainment::Outside => {}
            }
        }
    }
    let stamps = unique_map(&runtime.panes, |p| p.pane)?;
    let directories = unique_map(&affected.panes, |p| p.pane)?;
    if stamps.len() != state.panes.len() || directories.len() != state.panes.len() {
        return Err(GitFailure::Unchecked);
    }
    let phases: BTreeMap<_, _> = organization.runtime().iter().map(|r| (r.pane, r)).collect();
    let mut affected_tabs: BTreeSet<_> = state
        .tabs
        .iter()
        .filter(|t| t.worktree == Some(current.id))
        .map(|t| t.id)
        .collect();
    for pane in &state.panes {
        let stamp = stamps.get(&pane.id).ok_or(GitFailure::Unchecked)?;
        let phase = phases.get(&pane.id).ok_or(GitFailure::Unchecked)?;
        let (session, starting) = match phase.phase {
            SessionPhase::Running { generation } => (
                Some(SessionKey {
                    epoch: organization.epoch(),
                    pane: pane.id,
                    generation,
                }),
                false,
            ),
            SessionPhase::Starting { .. } => (None, true),
            _ => (None, false),
        };
        if stamp.session != session || stamp.startup_reserved != starting {
            return Err(GitFailure::StaleAssessment);
        }
        let directory = match &stamp.confirmed_directory {
            Some(observation) if Some(observation.session()) == session => observation.directory(),
            Some(_) => return Err(GitFailure::StaleAssessment),
            None => pane.restoration_directory(),
        };
        let containment = directories.get(&pane.id).ok_or(GitFailure::Unchecked)?;
        if &containment.directory != directory {
            return Err(GitFailure::Unchecked);
        }
        match containment.containment {
            DirectoryContainment::Inside => {
                affected_tabs.insert(pane.tab);
            }
            DirectoryContainment::Unchecked => return Err(GitFailure::Unchecked),
            DirectoryContainment::Outside => {}
        }
    }
    if affected_tabs.len() > MAX_GIT_TABS {
        return Err(GitFailure::Capacity);
    }
    let mut tabs: Vec<_> = state
        .tabs
        .iter()
        .filter(|t| affected_tabs.contains(&t.id))
        .cloned()
        .collect();
    tabs.sort_by_key(|t| t.id);
    let tab_map: BTreeMap<_, _> = tabs.iter().map(|t| (t.id, t)).collect();
    let mut panes = Vec::new();
    let mut retained_directories = Vec::new();
    for pane in &state.panes {
        if let Some(tab) = tab_map.get(&pane.tab) {
            if panes.len() == MAX_GIT_PANES {
                return Err(GitFailure::Capacity);
            }
            let stamp = stamps[&pane.id];
            let phase = phases[&pane.id];
            panes.push(RetainedAffectedPane {
                hierarchy: AssessedPane {
                    workspace: tab.workspace,
                    tab: tab.id,
                    pane: pane.id,
                    session_generation: phase.session_generation,
                    control_generation: phase.control_generation,
                    context_generation: stamp.context_generation,
                },
                session: stamp.session,
                startup_reserved: stamp.startup_reserved,
                work_generation: stamp.work_generation,
            });
            retained_directories.push((*directories[&pane.id]).clone());
        }
    }
    panes.sort_by_key(|p| p.hierarchy.pane);
    retained_directories.sort_by_key(|p| p.pane);
    let mut fallbacks = Vec::new();
    for workspace in &state.workspaces {
        if tabs.iter().any(|t| t.workspace == workspace.id)
            && state
                .tabs
                .iter()
                .filter(|t| t.workspace == workspace.id)
                .all(|t| affected_tabs.contains(&t.id))
        {
            let eligible = |fallback: &&FallbackWorktree| {
                fallback.record.repository == current.repository
                    && fallback.record.id != current.id
                    && !fallback.record.missing
                    && !fallback.bare
                    && fallback.available
                    && fallback.containment == DirectoryContainment::Outside
                    && state.worktrees.iter().any(|w| w == &fallback.record)
            };
            let fallback = affected
                .fallbacks
                .iter()
                .filter(eligible)
                .find(|f| f.record.main)
                .or_else(|| affected.fallbacks.iter().find(eligible))
                .ok_or(GitFailure::NoFallback)?;
            fallbacks.push(WorkspaceFallbackPlan {
                workspace: workspace.id,
                worktree: fallback.record.id,
                directory: fallback.record.directory.clone(),
            });
        }
    }
    fallbacks.sort_by_key(|f| f.workspace);
    let retained = RetainedRemovalScope::new(RetainedRemovalScopeParts {
        server: state.server,
        tabs,
        panes,
        fallbacks,
    })?;
    Ok(WorktreeRemovalScope {
        epoch: organization.epoch(),
        facts: target.clone(),
        retained,
        directories: retained_directories,
    })
}

/// Recompute exact affected work while allowing unrelated organization revisions.
pub fn revalidate_removal(
    assessment: &WorktreeRemovalScope,
    organization: &OrganizationSnapshot,
    runtime: &RuntimeWorkSet,
    affected: &AffectedWorktreeWork,
) -> Result<(), GitFailure> {
    let fresh = assess_removal(organization, runtime, &assessment.facts, affected)?;
    if &fresh != assessment {
        Err(GitFailure::StaleAssessment)
    } else {
        Ok(())
    }
}
pub fn revalidate_git_facts(
    assessment: &WorktreeRemovalScope,
    current: &WorktreeRemovalFacts,
) -> Result<(), GitFailure> {
    if &assessment.facts == current {
        Ok(())
    } else {
        Err(GitFailure::GitStateChanged)
    }
}
fn unique_map<T, K: Ord>(
    values: &[T],
    key: impl Fn(&T) -> K,
) -> Result<BTreeMap<K, &T>, GitFailure> {
    let mut map = BTreeMap::new();
    for value in values {
        if map.insert(key(value), value).is_some() {
            return Err(GitFailure::InvalidResponse);
        }
    }
    Ok(map)
}
