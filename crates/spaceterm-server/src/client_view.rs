//! Client-owned arrangements. These records never enter the protocol or server organization.
use crate::ServerError;
use crate::hierarchy::{Axis, Id, MAX_PANES, MAX_TABS, MAX_WORKSPACES, Tab, Workspace};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientView {
    pub workspaces: Vec<WorkspaceView>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceView {
    pub workspace: Id,
    /// Display order, independent of the server's Root Tab.
    pub tabs: Vec<TabView>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TabView {
    pub tab: Id,
    pub layout: Layout,
}

/// A flat tree avoids a serialization depth limit for deeply nested personal layouts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Layout {
    pub nodes: Vec<LayoutNode>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum LayoutNode {
    Pane(Id),
    Split {
        axis: Axis,
        ratio: f32,
        first: usize,
        second: usize,
    },
}

impl WorkspaceView {
    /// Keep surviving Tabs in personal order and append creations in server creation order.
    pub fn reconcile(&self, workspace: &Workspace) -> Self {
        let mut tabs = Vec::new();
        for saved in &self.tabs {
            if let Some(tab) = workspace.tabs.iter().find(|tab| tab.id == saved.tab) {
                tabs.push(TabView {
                    tab: tab.id,
                    layout: saved.layout.reconcile(tab),
                });
            }
        }
        for tab in &workspace.tabs {
            if !tabs.iter().any(|saved| saved.tab == tab.id) {
                tabs.push(TabView {
                    tab: tab.id,
                    layout: Layout { nodes: Vec::new() }.reconcile(tab),
                });
            }
        }
        Self {
            workspace: workspace.id,
            tabs,
        }
    }
}

// Only validated trees are traversed. Persisted child indexes must point forward and every node
// has exactly one parent, bounding both traversal and recursion by the Pane count.
impl Layout {
    fn tree(&self, index: usize, keep: &impl Fn(Id) -> bool) -> Option<Tree> {
        match self.nodes.get(index)? {
            LayoutNode::Pane(id) => keep(*id).then_some(Tree::Pane(*id)),
            LayoutNode::Split {
                axis,
                ratio,
                first,
                second,
            } => match (self.tree(*first, keep), self.tree(*second, keep)) {
                (Some(a), Some(b)) => Some(Tree::Split(*axis, *ratio, Box::new(a), Box::new(b))),
                (Some(tree), None) | (None, Some(tree)) => Some(tree),
                (None, None) => None,
            },
        }
    }
    pub fn validate(&self) -> Result<(), ServerError> {
        if self.nodes.is_empty() || self.nodes.len() > 2 * MAX_PANES - 1 {
            return Err(ServerError::UnsafeStorage);
        }
        let mut parents = vec![0; self.nodes.len()];
        let mut panes = BTreeSet::new();
        for (index, node) in self.nodes.iter().enumerate() {
            match node {
                LayoutNode::Pane(id) if !panes.insert(*id) => {
                    return Err(ServerError::UnsafeStorage);
                }
                LayoutNode::Pane(_) => {}
                LayoutNode::Split {
                    ratio,
                    first,
                    second,
                    ..
                } => {
                    if !ratio.is_finite()
                        || *ratio <= 0.0
                        || *ratio >= 1.0
                        || *first <= index
                        || *second <= index
                    {
                        return Err(ServerError::UnsafeStorage);
                    }
                    for child in [first, second] {
                        let count = parents.get_mut(*child).ok_or(ServerError::UnsafeStorage)?;
                        *count += 1;
                    }
                }
            }
        }
        if parents[0] != 0 || parents[1..].iter().any(|count| *count != 1) {
            return Err(ServerError::UnsafeStorage);
        }
        Ok(())
    }
    /// Remove deleted leaves, collapse their Splits, and insert new Panes beside surviving sources.
    pub fn reconcile(&self, tab: &Tab) -> Self {
        let mut tree = if self.validate().is_ok() {
            self.tree(0, &|id| tab.panes.iter().any(|pane| pane.id == id))
        } else {
            None
        };
        for pane in &tab.panes {
            match tree.as_mut() {
                Some(tree) if !tree.contains(pane.id) => {
                    let source = pane
                        .source
                        .filter(|id| tree.contains(*id))
                        .unwrap_or_else(|| {
                            if tree.contains(tab.root_pane) {
                                tab.root_pane
                            } else {
                                tree.first()
                            }
                        });
                    tree.insert(source, pane.id, pane.axis);
                }
                None => tree = Some(Tree::Pane(pane.id)),
                _ => {}
            }
        }
        let mut nodes = Vec::new();
        if let Some(tree) = tree {
            tree.flatten(&mut nodes);
        }
        Self { nodes }
    }
}

enum Tree {
    Pane(Id),
    Split(Axis, f32, Box<Tree>, Box<Tree>),
}
impl Tree {
    fn contains(&self, id: Id) -> bool {
        match self {
            Self::Pane(pane) => *pane == id,
            Self::Split(_, _, a, b) => a.contains(id) || b.contains(id),
        }
    }
    fn first(&self) -> Id {
        match self {
            Self::Pane(id) => *id,
            Self::Split(_, _, a, _) => a.first(),
        }
    }
    fn insert(&mut self, source: Id, id: Id, axis: Axis) {
        match self {
            Self::Pane(pane) if *pane == source => {
                *self = Self::Split(
                    axis,
                    0.5,
                    Box::new(Self::Pane(source)),
                    Box::new(Self::Pane(id)),
                )
            }
            Self::Split(_, _, a, b) => {
                a.insert(source, id, axis);
                b.insert(source, id, axis);
            }
            _ => {}
        }
    }
    fn flatten(&self, nodes: &mut Vec<LayoutNode>) -> usize {
        let index = nodes.len();
        nodes.push(LayoutNode::Pane(self.first()));
        nodes[index] = match self {
            Self::Pane(id) => LayoutNode::Pane(*id),
            Self::Split(axis, ratio, a, b) => LayoutNode::Split {
                axis: *axis,
                ratio: *ratio,
                first: a.flatten(nodes),
                second: b.flatten(nodes),
            },
        };
        index
    }
}
impl ClientView {
    pub fn validate(&self) -> Result<(), ServerError> {
        let mut workspaces = BTreeSet::new();
        let mut tabs = BTreeSet::new();
        let mut panes = BTreeSet::new();
        for workspace in &self.workspaces {
            if !workspaces.insert(workspace.workspace) {
                return Err(ServerError::UnsafeStorage);
            }
            for tab in &workspace.tabs {
                if !tabs.insert(tab.tab) {
                    return Err(ServerError::UnsafeStorage);
                }
                tab.layout.validate()?;
                for node in &tab.layout.nodes {
                    if let LayoutNode::Pane(id) = node
                        && !panes.insert(*id)
                    {
                        return Err(ServerError::UnsafeStorage);
                    }
                }
            }
        }
        if workspaces.len() > MAX_WORKSPACES || tabs.len() > MAX_TABS || panes.len() > MAX_PANES {
            return Err(ServerError::UnsafeStorage);
        }
        Ok(())
    }
}

#[cfg(feature = "client")]
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u32,
    pin: Id,
    device: Id,
    view: ClientView,
}

/// Separate from Settings and shared organization; its lock admits one Workspace Window per client.
#[cfg(feature = "client")]
pub struct ClientViewStore {
    directory: crate::storage::Directory,
    _lock: ClientViewLock,
    record: Record,
    blocked: bool,
}
#[cfg(feature = "client")]
struct ClientViewLock(std::fs::File);

#[cfg(feature = "client")]
impl ClientViewStore {
    pub(crate) fn open(
        directory: crate::storage::Directory,
        pin: Id,
        device: Id,
    ) -> Result<Self, ServerError> {
        let lock = ClientViewLock(directory.lock("view.lock", true)?);
        let record = directory
            .read_json::<Record>("view.json")?
            .unwrap_or(Record {
                version: 1,
                pin,
                device,
                view: ClientView::default(),
            });
        if record.version != 1 || record.pin != pin || record.device != device {
            return Err(ServerError::AuthenticationRefused);
        }
        record.view.validate()?;
        Ok(Self {
            directory,
            _lock: lock,
            record,
            blocked: false,
        })
    }
    pub fn view(&self) -> &ClientView {
        &self.record.view
    }
    pub fn save(&mut self, view: ClientView) -> Result<(), ServerError> {
        if self.blocked {
            return Err(ServerError::StorageUnavailable);
        }
        view.validate()?;
        if self.record.view == view {
            return Ok(());
        }
        self.record.view = view;
        if let Err(error) = self.directory.write_json(c"view.json", &self.record) {
            // An uncertain publication must be revalidated on reopen before another write.
            self.blocked = true;
            return Err(error);
        }
        Ok(())
    }
}
#[cfg(feature = "client")]
impl Drop for ClientViewLock {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;
        // SAFETY: this guard owns the live descriptor. Unlock on rejected records too:
        // another thread's fork may retain the descriptor until its child executes.
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hierarchy::Pane;
    fn pane(id: u8, source: Option<u8>, axis: Axis) -> Pane {
        Pane {
            id: [id; 32],
            source: source.map(|v| [v; 32]),
            axis,
            starting_directory: "/tmp".into(),
            confirmed_directory: None,
            creator: None,
        }
    }
    fn tab(id: u8, panes: Vec<Pane>) -> Tab {
        Tab {
            worktree: None,
            worktree_membership_fixed: false,
            worktree_repository: None,
            id: [id; 32],
            root_pane: panes[0].id,
            panes,
        }
    }
    fn workspace(tabs: Vec<Tab>) -> Workspace {
        Workspace {
            id: [1; 32],
            name: "work".into(),
            name_version: 1,
            pin_version: 1,
            pinned_directory: None,
            root_tab: tabs[0].id,
            tabs,
        }
    }
    #[test]
    fn returning_client_preserves_order_and_layout_while_reconciling_membership() {
        let saved = WorkspaceView {
            workspace: [1; 32],
            tabs: vec![
                TabView {
                    tab: [3; 32],
                    layout: Layout {
                        nodes: vec![
                            LayoutNode::Split {
                                axis: Axis::Down,
                                ratio: 0.7,
                                first: 1,
                                second: 2,
                            },
                            LayoutNode::Pane([6; 32]),
                            LayoutNode::Pane([5; 32]),
                        ],
                    },
                },
                TabView {
                    tab: [2; 32],
                    layout: Layout {
                        nodes: vec![LayoutNode::Pane([4; 32])],
                    },
                },
            ],
        };
        let current = workspace(vec![
            tab(2, vec![pane(4, None, Axis::Right)]),
            tab(
                3,
                vec![
                    pane(5, None, Axis::Right),
                    pane(6, Some(5), Axis::Right),
                    pane(7, Some(5), Axis::Down),
                ],
            ),
            tab(8, vec![pane(9, None, Axis::Right)]),
        ]);
        let actual = saved.reconcile(&current);
        assert_eq!(
            actual.tabs.iter().map(|t| t.tab).collect::<Vec<_>>(),
            vec![[3; 32], [2; 32], [8; 32]]
        );
        assert_eq!(
            actual.tabs[0].layout.nodes,
            vec![
                LayoutNode::Split {
                    axis: Axis::Down,
                    ratio: 0.7,
                    first: 1,
                    second: 2
                },
                LayoutNode::Pane([6; 32]),
                LayoutNode::Split {
                    axis: Axis::Down,
                    ratio: 0.5,
                    first: 3,
                    second: 4
                },
                LayoutNode::Pane([5; 32]),
                LayoutNode::Pane([7; 32])
            ]
        );
    }
}

#[cfg(test)]
mod reconciliation_tests {
    use super::*;
    use crate::hierarchy::Pane;
    #[test]
    fn deleted_split_sources_leave_a_valid_default_and_never_return_from_saved_layout() {
        let saved = Layout {
            nodes: vec![
                LayoutNode::Split {
                    axis: Axis::Right,
                    ratio: 0.8,
                    first: 1,
                    second: 2,
                },
                LayoutNode::Pane([1; 32]),
                LayoutNode::Pane([2; 32]),
            ],
        };
        let tab = Tab {
            worktree: None,
            worktree_membership_fixed: false,
            worktree_repository: None,
            id: [3; 32],
            root_pane: [2; 32],
            panes: vec![
                Pane {
                    id: [2; 32],
                    source: None,
                    axis: Axis::Right,
                    starting_directory: "/tmp".into(),
                    confirmed_directory: None,
                    creator: None,
                },
                Pane {
                    id: [4; 32],
                    source: None,
                    axis: Axis::Down,
                    starting_directory: "/tmp".into(),
                    confirmed_directory: None,
                    creator: None,
                },
            ],
        };
        let returning = saved.reconcile(&tab);
        let fresh = Layout { nodes: Vec::new() }.reconcile(&tab);
        let expected = vec![
            LayoutNode::Split {
                axis: Axis::Down,
                ratio: 0.5,
                first: 1,
                second: 2,
            },
            LayoutNode::Pane([2; 32]),
            LayoutNode::Pane([4; 32]),
        ];
        assert_eq!((returning.nodes, fresh.nodes), (expected.clone(), expected));
    }
    #[test]
    fn retained_layout_rejects_cycles_shared_children_duplicate_panes_and_invalid_ratios() {
        for nodes in [
            vec![
                LayoutNode::Split {
                    axis: Axis::Right,
                    ratio: 0.5,
                    first: 0,
                    second: 1,
                },
                LayoutNode::Pane([1; 32]),
            ],
            vec![
                LayoutNode::Split {
                    axis: Axis::Right,
                    ratio: 0.5,
                    first: 1,
                    second: 1,
                },
                LayoutNode::Pane([1; 32]),
            ],
            vec![
                LayoutNode::Split {
                    axis: Axis::Right,
                    ratio: 0.5,
                    first: 1,
                    second: 2,
                },
                LayoutNode::Pane([1; 32]),
                LayoutNode::Pane([1; 32]),
            ],
            vec![
                LayoutNode::Split {
                    axis: Axis::Right,
                    ratio: f32::NAN,
                    first: 1,
                    second: 2,
                },
                LayoutNode::Pane([1; 32]),
                LayoutNode::Pane([2; 32]),
            ],
        ] {
            assert_eq!(Layout { nodes }.validate(), Err(ServerError::UnsafeStorage));
        }
    }
}
