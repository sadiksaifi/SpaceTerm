use std::collections::{BTreeMap, BTreeSet};

use super::contracts::*;
use super::state::{ReducerState, ReductionPatch};
use crate::identity::*;
use crate::operations::Outcome;
use crate::organization::*;
use crate::organization_retained::*;

#[derive(Clone, Copy)]
enum Node {
    Pane(PaneId),
    Split {
        axis: SplitAxis,
        first: usize,
        second: usize,
    },
}
type NormalizedDefault = (TabCreationDefault, BTreeMap<PaneId, PaneCreation>);
struct Tree {
    nodes: Vec<Node>,
    root: usize,
}
impl Tree {
    fn from_default(
        default: &TabCreationDefault,
        panes: &[PaneRecord],
    ) -> Result<Self, ReducerFailure> {
        default
            .validate(panes)
            .map_err(|_| ReducerFailure::InvalidFacts)?;
        let mut tree = Self {
            nodes: Vec::new(),
            root: 0,
        };
        let mut leaves = BTreeMap::new();
        let records: BTreeMap<_, _> = panes
            .iter()
            .filter(|v| v.tab == default.tab())
            .map(|v| (v.id, v))
            .collect();
        for id in default.replay_order() {
            let record = records.get(id).ok_or(ReducerFailure::InvalidFacts)?;
            match record.creation {
                PaneCreation::Initial => {
                    leaves.insert(*id, 0);
                    tree.nodes.push(Node::Pane(*id));
                }
                PaneCreation::SplitOf { source, axis, edge } => {
                    let index = *leaves.get(&source).ok_or(ReducerFailure::InvalidFacts)?;
                    let old = tree.nodes.len();
                    let new = old.checked_add(1).ok_or(ReducerFailure::CounterExhausted)?;
                    tree.nodes.extend([Node::Pane(source), Node::Pane(*id)]);
                    let (first, second) = if edge == SplitEdge::Before {
                        (new, old)
                    } else {
                        (old, new)
                    };
                    tree.nodes[index] = Node::Split {
                        axis,
                        first,
                        second,
                    };
                    leaves.insert(source, old);
                    leaves.insert(*id, new);
                }
            }
        }
        Ok(tree)
    }

    fn remove(self, removed: &BTreeSet<PaneId>) -> Option<Self> {
        let mut mapping = vec![None; self.nodes.len()];
        let mut nodes = Vec::new();
        // Child indices exceed their parents' indices. Reverse traversal is a
        // bounded postorder, including pathological one-sided split histories.
        for (index, node) in self.nodes.iter().enumerate().rev() {
            mapping[index] = match *node {
                Node::Pane(id) if removed.contains(&id) => None,
                Node::Pane(id) => {
                    nodes.push(Node::Pane(id));
                    Some(nodes.len() - 1)
                }
                Node::Split {
                    axis,
                    first,
                    second,
                } => match (mapping[first], mapping[second]) {
                    (Some(first), Some(second)) => {
                        nodes.push(Node::Split {
                            axis,
                            first,
                            second,
                        });
                        Some(nodes.len() - 1)
                    }
                    (Some(child), None) | (None, Some(child)) => Some(child),
                    (None, None) => None,
                },
            };
        }
        mapping[self.root].map(|root| Self { nodes, root })
    }

    fn instructions(
        &self,
        tab: TabId,
    ) -> Result<(TabCreationDefault, BTreeMap<PaneId, PaneCreation>), ReducerFailure> {
        let mut anchors = vec![None; self.nodes.len()];
        // The pruned tree was built in postorder, so children precede parents.
        for (index, node) in self.nodes.iter().enumerate() {
            anchors[index] = match *node {
                Node::Pane(id) => Some(id),
                Node::Split { first, .. } => anchors[first],
            };
        }
        let initial = anchors[self.root].ok_or(ReducerFailure::InvalidFacts)?;
        let mut order = vec![initial];
        let mut creations = BTreeMap::from([(initial, PaneCreation::Initial)]);
        let mut pending = vec![self.root];
        while let Some(index) = pending.pop() {
            if let Node::Split {
                axis,
                first,
                second,
            } = self.nodes[index]
            {
                let source = anchors[first].ok_or(ReducerFailure::InvalidFacts)?;
                let pane = anchors[second].ok_or(ReducerFailure::InvalidFacts)?;
                order.push(pane);
                if creations
                    .insert(
                        pane,
                        PaneCreation::SplitOf {
                            source,
                            axis,
                            edge: SplitEdge::After,
                        },
                    )
                    .is_some()
                {
                    return Err(ReducerFailure::InvalidFacts);
                }
                pending.push(second);
                pending.push(first);
            }
        }
        let default =
            TabCreationDefault::new(tab, order).map_err(|_| ReducerFailure::InvalidFacts)?;
        Ok((default, creations))
    }
}

fn normalized(
    state: &ReducerState,
    tab: TabId,
    removed: &[PaneId],
) -> Result<Option<NormalizedDefault>, ReducerFailure> {
    let facts = state
        .organization()
        .retained
        .as_ref()
        .ok_or(ReducerFailure::InvalidFacts)?;
    let default = facts
        .creation_defaults()
        .iter()
        .find(|v| v.tab() == tab)
        .ok_or(ReducerFailure::TargetNotFound)?;
    let tree = Tree::from_default(default, &state.organization().panes)?;
    tree.remove(&removed.iter().copied().collect())
        .map(|tree| tree.instructions(tab))
        .transpose()
}

/// Frozen shared helper. The canonical default is replay order, not another tree.
pub(crate) fn normalize_creation_default(
    state: &ReducerState,
    tab: TabId,
    removed: &[PaneId],
) -> Result<Option<TabCreationDefault>, ReducerFailure> {
    Ok(normalized(state, tab, removed)?.map(|(default, _)| default))
}

pub(crate) struct HierarchyRemovalPatch {
    pub(crate) candidate: ReducerState,
}
impl HierarchyRemovalPatch {
    pub(crate) fn into_reduction(self) -> ReductionPatch {
        ReductionPatch {
            candidate: self.candidate,
            effects: Vec::new(),
            resolution: Resolution::Completed(Outcome::Applied),
            operations: Vec::new(),
            accepted_creations: Vec::new(),
        }
    }
}

/// The only hierarchy cascade. It supplies no confirmation or cleanup policy.
/// B/C attach their exact checks, invalidation effects and pending descriptors.
pub(crate) fn remove_hierarchy(
    state: &ReducerState,
    panes: &[PaneId],
) -> Result<HierarchyRemovalPatch, ReducerFailure> {
    let removed: BTreeSet<_> = panes.iter().copied().collect();
    if removed.len() != panes.len() {
        return Err(ReducerFailure::InvalidFacts);
    }
    if removed
        .iter()
        .any(|id| id.server() != state.organization().server)
    {
        return Err(ReducerFailure::WrongServer);
    }
    if removed
        .iter()
        .any(|id| !state.organization().panes.iter().any(|v| v.id == *id))
    {
        return Err(ReducerFailure::TargetNotFound);
    }
    let mut candidate = state.clone();
    let mut org = state.organization().clone();
    let mut facts = org
        .retained
        .as_ref()
        .ok_or(ReducerFailure::InvalidFacts)?
        .parts()
        .clone();
    let affected: BTreeSet<_> = org
        .panes
        .iter()
        .filter(|v| removed.contains(&v.id))
        .map(|v| v.tab)
        .collect();
    for tab_id in &affected {
        // Calling the shared default helper also freezes its contract for B/C.
        let default = super::normalize_creation_default(state, *tab_id, panes)?;
        if let Some(default) = default {
            let (_, creations) =
                normalized(state, *tab_id, panes)?.ok_or(ReducerFailure::InvalidFacts)?;
            for pane in org
                .panes
                .iter_mut()
                .filter(|v| v.tab == *tab_id && !removed.contains(&v.id))
            {
                pane.creation = *creations
                    .get(&pane.id)
                    .ok_or(ReducerFailure::InvalidFacts)?;
            }
            facts.creation_defaults.retain(|v| v.tab() != *tab_id);
            facts.creation_defaults.push(default);
        } else {
            facts.creation_defaults.retain(|v| v.tab() != *tab_id);
        }
    }
    org.panes.retain(|v| !removed.contains(&v.id));
    facts
        .pane_generations
        .retain(|v| !removed.contains(&v.pane()));
    let old_tabs: BTreeMap<_, _> = org.tabs.iter().map(|v| (v.id, v.clone())).collect();
    org.tabs
        .retain(|tab| org.panes.iter().any(|v| v.tab == tab.id));
    for tab in &mut org.tabs {
        if removed.contains(&tab.root_pane) {
            tab.root_pane = org
                .panes
                .iter()
                .filter(|v| v.tab == tab.id)
                .min_by_key(|v| v.creation_ordinal)
                .ok_or(ReducerFailure::InvalidFacts)?
                .id;
        }
    }
    facts
        .tab_membership_versions
        .retain(|v| org.tabs.iter().any(|t| t.id == v.id()));
    org.workspaces
        .retain(|v| org.tabs.iter().any(|t| t.workspace == v.id));
    for workspace in &mut org.workspaces {
        if !org.tabs.iter().any(|v| v.id == workspace.root_tab) {
            let class = old_tabs
                .get(&workspace.root_tab)
                .ok_or(ReducerFailure::InvalidFacts)?
                .worktree;
            workspace.root_tab = org
                .tabs
                .iter()
                .filter(|v| v.workspace == workspace.id)
                .min_by_key(|v| (v.worktree != class, v.creation_ordinal))
                .ok_or(ReducerFailure::InvalidFacts)?
                .id;
        }
    }
    facts
        .workspace_names
        .retain(|v| org.workspaces.iter().any(|w| w.id == v.workspace()));
    facts.creation_defaults.sort_by_key(TabCreationDefault::tab);
    org.retained =
        Some(OrganizationRetainedFacts::new(facts).map_err(|_| ReducerFailure::InvalidFacts)?);
    super::naming::refresh_root_names(state, &mut org)?;
    let runtime = state
        .snapshot
        .runtime()
        .iter()
        .filter(|v| !removed.contains(&v.pane))
        .copied()
        .collect();
    candidate.observations.retain(|id, _| !removed.contains(id));
    candidate.replace_organization(org, runtime)?;
    Ok(HierarchyRemovalPatch { candidate })
}
