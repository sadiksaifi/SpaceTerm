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
    /// Target and destructive categories for every page of this token-bound confirmation.
    pub fn summary(&self) -> queries::GitRemovalSummary {
        queries::GitRemovalSummary {
            worktree: self.facts.target.id,
            directory: self.facts.target.directory.clone(),
            branch: match &self.facts.head {
                Some(RetainedGitHead::Branch { name, .. } | RetainedGitHead::Unborn(name)) => {
                    Some(name.clone())
                }
                _ => None,
            },
            risk: self.approval(),
        }
    }
    /// Hash the complete comparison scope, including authoritative directory facts.
    /// Native handles remain outside this portable digest.
    pub fn assessment_digest(&self) -> GitAssessmentDigest {
        use sha2::{Digest, Sha256};
        fn number(hash: &mut Sha256, value: u64) {
            hash.update(value.to_le_bytes());
        }
        fn bytes(hash: &mut Sha256, value: &[u8]) {
            number(hash, value.len() as u64);
            hash.update(value);
        }
        fn object(hash: &mut Sha256, object: &GitObjectId) {
            match object {
                GitObjectId::Sha1(value) => bytes(hash, value),
                GitObjectId::Sha256(value) => bytes(hash, value),
            }
        }
        let mut hash = Sha256::new();
        hash.update(b"spaceterm-worktree-removal-scope-v1\0");
        hash.update(self.retained.parts().server.bytes());
        number(&mut hash, self.epoch.get());
        let facts = &self.facts;
        number(&mut hash, facts.target.id.local());
        number(&mut hash, facts.target.repository.local());
        bytes(&mut hash, facts.target.directory.bytes());
        hash.update([
            facts.target.main as u8,
            facts.target.missing as u8,
            facts.listed as u8,
            facts.locked as u8,
            facts.changes as u8,
            facts.submodules as u8,
        ]);
        match &facts.head {
            None => number(&mut hash, 0),
            Some(RetainedGitHead::Unborn(name)) => {
                number(&mut hash, 1);
                bytes(&mut hash, name.as_str().as_bytes());
            }
            Some(RetainedGitHead::Detached(commit)) => {
                number(&mut hash, 2);
                object(&mut hash, commit);
            }
            Some(RetainedGitHead::Branch { name, commit }) => {
                number(&mut hash, 3);
                bytes(&mut hash, name.as_str().as_bytes());
                object(&mut hash, commit);
            }
        }
        let parts = self.retained.parts();
        number(&mut hash, parts.tabs.len() as u64);
        for tab in &parts.tabs {
            for value in [
                tab.workspace.local(),
                tab.id.local(),
                tab.root_pane.local(),
                tab.creation_ordinal.get(),
                tab.worktree.map_or(0, |id| id.local()),
            ] {
                number(&mut hash, value);
            }
        }
        number(&mut hash, parts.membership_versions.len() as u64);
        for version in &parts.membership_versions {
            number(&mut hash, version.id().local());
            number(&mut hash, version.version().get());
        }
        number(&mut hash, parts.panes.len() as u64);
        for pane in &parts.panes {
            let p = pane.hierarchy;
            for value in [
                p.workspace.local(),
                p.tab.local(),
                p.pane.local(),
                p.session_generation.get(),
                p.control_generation.get(),
                p.context_generation.get(),
                pane.work_generation.get(),
                pane.session.map_or(0, |s| s.epoch.get()),
                pane.startup_reserved as u64,
            ] {
                number(&mut hash, value);
            }
        }
        number(&mut hash, parts.fallbacks.len() as u64);
        for fallback in &parts.fallbacks {
            number(&mut hash, fallback.workspace.local());
            number(&mut hash, fallback.worktree.local());
            bytes(&mut hash, fallback.directory.bytes());
        }
        number(&mut hash, self.directories.len() as u64);
        for observation in &self.directories {
            number(&mut hash, observation.pane.local());
            bytes(&mut hash, observation.directory.bytes());
            number(
                &mut hash,
                match observation.containment {
                    DirectoryContainment::Inside => 1,
                    DirectoryContainment::Outside => 2,
                    DirectoryContainment::Unchecked => 3,
                },
            );
        }
        GitAssessmentDigest::from_bytes(hash.finalize().into())
    }
    /// Conservative cache residency using actual directory lengths, not the
    /// maximum directory bound multiplied by every affected Pane.
    pub fn value_bytes(&self) -> usize {
        let parts = self.retained.parts();
        2048 + self.facts.target.directory.bytes().len()
            + match &self.facts.head {
                Some(RetainedGitHead::Branch { name, .. } | RetainedGitHead::Unborn(name)) => {
                    name.as_str().len()
                }
                _ => 0,
            }
            + parts.tabs.len() * 192
            + parts.panes.len() * 256
            + self
                .directories
                .iter()
                .map(|p| 128 + p.directory.bytes().len())
                .sum::<usize>()
            + parts
                .fallbacks
                .iter()
                .map(|f| 128 + f.directory.bytes().len())
                .sum::<usize>()
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
    let membership_versions = if tabs.is_empty() {
        Vec::new()
    } else {
        let versions = state
            .retained
            .as_ref()
            .ok_or(GitFailure::Unchecked)?
            .tab_membership_versions();
        tabs.iter()
            .map(|tab| {
                versions
                    .binary_search_by_key(&tab.id, |v| v.id())
                    .map(|index| versions[index])
                    .map_err(|_| GitFailure::Unchecked)
            })
            .collect::<Result<Vec<_>, _>>()?
    };
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
        membership_versions,
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
