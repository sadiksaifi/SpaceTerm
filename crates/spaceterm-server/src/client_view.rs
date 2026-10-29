//! Client-owned arrangements. These records never enter the protocol or server organization.
use crate::ServerError;
use crate::hierarchy::{
    Axis, Id, Inventory, MAX_PANES, MAX_TABS, MAX_WORKSPACES, Tab, Workspace, WorktreeMembership,
};
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

/// The bound on one retained Workspace name, which the server applies to every name it accepts.
const MAX_NAME_BYTES: usize = 256;

/// The Workspaces and Tabs a client last presented from one server, kept so it can present them
/// while that server is not connected.
///
/// It holds identities, Workspace names, Split structure and each Pane's shell name only.
/// Program-set titles, directories, Worktrees, terminal contents, commands and live Session facts
/// never enter it, and it never supplies authority: the server's next inventory replaces it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetainedInventory {
    pub revision: u64,
    pub workspaces: Vec<RetainedWorkspace>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetainedWorkspace {
    pub id: Id,
    pub name: String,
    pub root_tab: Id,
    pub tabs: Vec<RetainedTab>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetainedTab {
    pub id: Id,
    pub root_pane: Id,
    pub panes: Vec<RetainedPane>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetainedPane {
    pub id: Id,
    pub source: Option<Id>,
    pub axis: Axis,
    /// The name of the Pane's shell, such as `zsh`. A title a program sets can name hosts,
    /// paths and commands, so it is never kept.
    pub shell: Option<String>,
}

impl RetainedInventory {
    /// The structure, Workspace names and shell names of `inventory`'s Workspaces.
    pub fn retain(inventory: &Inventory) -> Self {
        let shell = |pane: &crate::hierarchy::Pane| {
            let shell: String = pane
                .presentation
                .as_deref()?
                .shell_name
                .chars()
                .filter(|c| !c.is_control())
                .take(crate::pane::metadata::MAX_TITLE_CHARS)
                .collect();
            let shell = shell.trim();
            (!shell.is_empty()).then(|| shell.to_owned())
        };
        Self {
            revision: inventory.revision,
            workspaces: inventory
                .workspaces
                .iter()
                .map(|workspace| RetainedWorkspace {
                    id: workspace.id,
                    name: workspace.name.clone(),
                    root_tab: workspace.root_tab,
                    tabs: workspace
                        .tabs
                        .iter()
                        .map(|tab| RetainedTab {
                            id: tab.id,
                            root_pane: tab.root_pane,
                            panes: tab
                                .panes
                                .iter()
                                .map(|pane| RetainedPane {
                                    id: pane.id,
                                    source: pane.source,
                                    axis: pane.axis,
                                    shell: shell(pane),
                                })
                                .collect(),
                        })
                        .collect(),
                })
                .collect(),
        }
    }

    /// Holds the copy to the server's own bounds on Workspaces, Tabs, Panes and names, and the
    /// Terminal Metadata bound on shell names.
    pub fn validate(&self) -> Result<(), ServerError> {
        let invalid = || ServerError::UnsafeStorage;
        let text = |value: &str, bound: usize| {
            !value.trim().is_empty() && value.len() <= bound && !value.chars().any(char::is_control)
        };
        let mut ids = BTreeSet::new();
        let (mut tabs, mut panes) = (0_usize, 0_usize);
        if self.workspaces.len() > MAX_WORKSPACES {
            return Err(invalid());
        }
        for workspace in &self.workspaces {
            if !ids.insert(workspace.id)
                || !text(&workspace.name, MAX_NAME_BYTES)
                || !workspace
                    .tabs
                    .iter()
                    .any(|tab| tab.id == workspace.root_tab)
            {
                return Err(invalid());
            }
            tabs += workspace.tabs.len();
            for tab in &workspace.tabs {
                if !ids.insert(tab.id) || !tab.panes.iter().any(|pane| pane.id == tab.root_pane) {
                    return Err(invalid());
                }
                panes += tab.panes.len();
                for pane in &tab.panes {
                    if !ids.insert(pane.id)
                        || pane.shell.as_deref().is_some_and(|shell| {
                            !text(shell, 4 * crate::pane::metadata::MAX_TITLE_CHARS)
                                || shell.chars().count() > crate::pane::metadata::MAX_TITLE_CHARS
                        })
                    {
                        return Err(invalid());
                    }
                }
            }
        }
        if tabs > MAX_TABS || panes > MAX_PANES || ids.contains(&[0; 32]) {
            return Err(invalid());
        }
        Ok(())
    }

    /// The copy as an inventory a client can present. It names no directory and carries no live
    /// facts, so nothing it presents can open a Terminal Session or grant local authority.
    pub fn inventory(&self) -> Inventory {
        use crate::hierarchy::Pane;
        Inventory {
            repositories: None,
            revision: self.revision,
            workspaces: self
                .workspaces
                .iter()
                .map(|workspace| Workspace {
                    id: workspace.id,
                    name: workspace.name.clone(),
                    name_version: 0,
                    pin_version: 0,
                    pinned_directory: None,
                    root_tab: workspace.root_tab,
                    tabs: workspace
                        .tabs
                        .iter()
                        .map(|tab| Tab {
                            worktree_membership: WorktreeMembership::Following,
                            worktree: None,
                            worktree_repository: None,
                            id: tab.id,
                            root_pane: tab.root_pane,
                            panes: tab
                                .panes
                                .iter()
                                .map(|pane| Pane {
                                    id: pane.id,
                                    source: pane.source,
                                    axis: pane.axis,
                                    starting_directory: std::path::PathBuf::new(),
                                    current_directory: None,
                                    repository_directory: None,
                                    presentation: None,
                                    observers: Default::default(),
                                    confirmed_directory: None,
                                    creator: None,
                                })
                                .collect(),
                        })
                        .collect(),
                })
                .collect(),
            close: None,
        }
    }

    /// The shell name retained for one Pane.
    pub fn shell(&self, pane: Id) -> Option<&str> {
        self.workspaces
            .iter()
            .flat_map(|workspace| &workspace.tabs)
            .flat_map(|tab| &tab.panes)
            .find(|retained| retained.id == pane)?
            .shell
            .as_deref()
    }

    /// Whether `other` has the same Workspaces, Tabs, Panes, Splits and names. Shell names and
    /// the revision are not structure.
    pub fn same_structure(&self, other: &Self) -> bool {
        let structure = |copy: &Self| {
            let mut copy = copy.clone();
            copy.revision = 0;
            for pane in copy
                .workspaces
                .iter_mut()
                .flat_map(|workspace| &mut workspace.tabs)
                .flat_map(|tab| &mut tab.panes)
            {
                pane.shell = None;
            }
            copy
        };
        structure(self) == structure(other)
    }
}

#[cfg(feature = "client")]
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u32,
    server: Id,
    client: Id,
    view: ClientView,
}

#[cfg(feature = "client")]
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InventoryRecord {
    version: u32,
    server: Id,
    client: Id,
    inventory: RetainedInventory,
}

#[cfg(feature = "client")]
const INVENTORY: &std::ffi::CStr = c"inventory.json";

/// The revision of the inventory retained in one client directory for `server` and `client`,
/// read without the client views' lock. A copy that can't be read has none.
#[cfg(feature = "client")]
pub(crate) fn retained_revision(
    directory: &crate::storage::Directory,
    server: Id,
    client: Id,
) -> Option<u64> {
    directory
        .read_json::<InventoryRecord>("inventory.json")
        .ok()
        .flatten()
        .filter(|retained| {
            retained.version == 1
                && retained.server == server
                && retained.client == client
                && retained.inventory.validate().is_ok()
        })
        .map(|retained| retained.inventory.revision)
}

/// Removes the retained inventory in one client directory.
#[cfg(feature = "client")]
pub(crate) fn discard_inventory(directory: &crate::storage::Directory) -> Result<(), ServerError> {
    directory.remove(INVENTORY)
}

/// Separate from Settings and shared organization; its lock admits one Workspace Window per client.
#[cfg(feature = "client")]
pub struct ClientViewStore {
    directory: crate::storage::Directory,
    _lock: crate::storage::Lock,
    record: Record,
    blocked: bool,
    /// The retained inventory as last read or written. A copy that can't be read is absent: it
    /// only ever stands in for the server, whose next inventory replaces it.
    inventory: Option<RetainedInventory>,
}

#[cfg(feature = "client")]
impl ClientViewStore {
    pub(crate) fn open(
        directory: crate::storage::Directory,
        server: Id,
        client: Id,
    ) -> Result<Self, ServerError> {
        let lock = directory.lock("view.lock", true)?;
        let record = directory
            .read_json::<Record>("view.json")?
            .unwrap_or(Record {
                version: 1,
                server,
                client,
                view: ClientView::default(),
            });
        if record.version != 1 || record.server != server || record.client != client {
            return Err(ServerError::AuthenticationRefused);
        }
        record.view.validate()?;
        let inventory = directory
            .read_json::<InventoryRecord>("inventory.json")
            .ok()
            .flatten()
            .filter(|retained| {
                retained.version == 1
                    && retained.server == server
                    && retained.client == client
                    && retained.inventory.validate().is_ok()
            })
            .map(|retained| retained.inventory);
        Ok(Self {
            directory,
            _lock: lock,
            record,
            blocked: false,
            inventory,
        })
    }
    /// The Workspaces and Tabs this client last presented from the server, if it retains them.
    pub fn retained_inventory(&self) -> Option<&RetainedInventory> {
        self.inventory.as_ref()
    }
    /// Replaces the retained inventory. A copy outside its bounds is refused and the previous
    /// one stays.
    pub fn save_inventory(&mut self, inventory: RetainedInventory) -> Result<(), ServerError> {
        inventory.validate()?;
        if self.inventory.as_ref() == Some(&inventory) {
            return Ok(());
        }
        self.directory.write_json(
            INVENTORY,
            &InventoryRecord {
                version: 1,
                server: self.record.server,
                client: self.record.client,
                inventory: inventory.clone(),
            },
        )?;
        self.inventory = Some(inventory);
        Ok(())
    }
    /// Removes the retained inventory, as when the route that kept it is removed.
    pub fn discard_inventory(&mut self) -> Result<(), ServerError> {
        self.inventory = None;
        discard_inventory(&self.directory)
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
            current_directory: None,
            repository_directory: None,
            presentation: None,
            observers: Default::default(),
            confirmed_directory: None,
            creator: None,
        }
    }
    fn tab(id: u8, panes: Vec<Pane>) -> Tab {
        Tab {
            worktree: None,
            worktree_membership: WorktreeMembership::Following,
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
            worktree_membership: WorktreeMembership::Following,
            worktree_repository: None,
            id: [3; 32],
            root_pane: [2; 32],
            panes: vec![
                Pane {
                    id: [2; 32],
                    source: None,
                    axis: Axis::Right,
                    starting_directory: "/tmp".into(),
                    current_directory: None,
                    repository_directory: None,
                    presentation: None,
                    observers: Default::default(),
                    confirmed_directory: None,
                    creator: None,
                },
                Pane {
                    id: [4; 32],
                    source: None,
                    axis: Axis::Down,
                    starting_directory: "/tmp".into(),
                    current_directory: None,
                    repository_directory: None,
                    presentation: None,
                    observers: Default::default(),
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

#[cfg(test)]
mod retained_inventory_tests {
    use super::*;
    use crate::hierarchy::Pane;
    use crate::pane::metadata::{
        Command, CommandState, DirectoryProvenance, Freshness, MAX_TITLE_CHARS, Progress,
        PromptZone, Snapshot, TitleGlyph, TitleProvenance,
    };

    fn presentation(title: &str, provenance: TitleProvenance) -> Box<Snapshot> {
        Box::new(Snapshot {
            revision: 1,
            shell_name: "zsh".into(),
            title: title.into(),
            title_provenance: provenance,
            directory: "/home/tester/secret".into(),
            directory_provenance: DirectoryProvenance::Osc7,
            user: Some("tester".into()),
            host: Some("server".into()),
            home: Some("/home/tester".into()),
            freshness: Freshness::Live,
            finished_commands: 3,
            prompt_zone: PromptZone::CommandOutput,
            command: Some(Command {
                line: "export TOKEN=secret".into(),
                state: CommandState::Running,
            }),
            title_activity: false,
            title_glyph: TitleGlyph::Reported,
            progress: Progress::None,
        })
    }

    fn pane(id: u8, presentation: Option<Box<Snapshot>>) -> Pane {
        Pane {
            id: [id; 32],
            source: None,
            axis: Axis::Down,
            starting_directory: "/home/tester/project".into(),
            current_directory: Some("/home/tester/project/src".into()),
            repository_directory: Some("/home/tester/project".into()),
            presentation,
            observers: Default::default(),
            confirmed_directory: Some("/home/tester/project".into()),
            creator: Some([9; 32]),
        }
    }

    fn inventory(panes: Vec<Pane>) -> Inventory {
        Inventory {
            repositories: None,
            revision: 7,
            workspaces: vec![Workspace {
                id: [1; 32],
                name: "Work".into(),
                name_version: 3,
                pin_version: 2,
                pinned_directory: Some("/home/tester/project".into()),
                root_tab: [2; 32],
                tabs: vec![Tab {
                    worktree_membership: WorktreeMembership::Opened,
                    worktree: Some("/home/tester/project".into()),
                    worktree_repository: Some("/home/tester/project".into()),
                    id: [2; 32],
                    root_pane: panes[0].id,
                    panes,
                }],
            }],
            close: None,
        }
    }

    #[test]
    fn a_retained_copy_keeps_structure_and_shell_names_and_drops_titles_paths_and_commands() {
        let mut published = inventory(vec![
            pane(
                3,
                Some(presentation("vim notes", TitleProvenance::TerminalControl)),
            ),
            pane(
                4,
                Some(presentation("~/project", TitleProvenance::WorkingDirectory)),
            ),
            pane(5, None),
        ]);

        published.workspaces[0].tabs[0].panes[0].observers = crate::hierarchy::PaneObservers {
            names: vec!["Observer CLI".into()],
            others: 4,
        };
        let retained = RetainedInventory::retain(&published);
        let presented = retained.inventory();
        let stored = serde_json::to_string(&retained).unwrap();

        assert_eq!(retained.validate(), Ok(()));
        assert_eq!(
            (
                retained.shell([3; 32]),
                retained.shell([4; 32]),
                retained.shell([5; 32])
            ),
            (Some("zsh"), Some("zsh"), None)
        );
        for secret in [
            "vim notes",
            "/home/tester",
            "TOKEN",
            "tester",
            "server",
            "Observer CLI",
        ] {
            assert!(!stored.contains(secret), "the copy holds {secret:?}");
        }
        let workspace = &presented.workspaces[0];
        assert_eq!(
            (
                workspace.name.as_str(),
                workspace.pinned_directory.as_ref(),
                workspace.tabs[0].worktree.as_ref(),
                presented.revision,
            ),
            ("Work", None, None, 7)
        );
        assert!(workspace.tabs[0].panes.iter().all(|pane| {
            pane.starting_directory.as_os_str().is_empty()
                && pane.presentation.is_none()
                && pane.observers == crate::hierarchy::PaneObservers::default()
                && pane.creator.is_none()
        }));
    }

    #[test]
    fn a_retained_copy_stays_within_the_server_bounds() {
        let long = "z".repeat(MAX_TITLE_CHARS * 2);
        let mut published = presentation("vim", TitleProvenance::TerminalControl);
        published.shell_name.clone_from(&long);
        let retained = RetainedInventory::retain(&inventory(vec![pane(3, Some(published))]));
        assert_eq!(
            retained.shell([3; 32]).map(|shell| shell.chars().count()),
            Some(MAX_TITLE_CHARS),
            "a long shell name is cut to the Terminal Metadata bound"
        );
        assert_eq!(retained.validate(), Ok(()));

        let mut oversized = retained.clone();
        oversized.workspaces[0].tabs[0].panes[0].shell = Some(long);
        let mut unnamed = retained.clone();
        unnamed.workspaces[0].name = " ".into();
        let mut too_many_panes = retained.clone();
        too_many_panes.workspaces[0].tabs[0].panes = (0..=MAX_PANES)
            .map(|index| RetainedPane {
                id: [u8::try_from(index % 250).unwrap() + 3, 1, 2, 3]
                    .into_iter()
                    .chain(std::iter::repeat_n(u8::try_from(index / 250).unwrap(), 28))
                    .collect::<Vec<_>>()
                    .try_into()
                    .unwrap(),
                source: None,
                axis: Axis::Right,
                shell: None,
            })
            .collect();
        too_many_panes.workspaces[0].tabs[0].root_pane =
            too_many_panes.workspaces[0].tabs[0].panes[0].id;
        let mut duplicate = retained.clone();
        let copy = duplicate.workspaces[0].clone();
        duplicate.workspaces.push(copy);
        for invalid in [oversized, unnamed, too_many_panes, duplicate] {
            assert_eq!(invalid.validate(), Err(ServerError::UnsafeStorage));
        }
    }

    #[test]
    fn a_shell_name_or_revision_alone_is_no_structural_change() {
        let published = inventory(vec![pane(3, None), pane(4, None)]);
        let retained = RetainedInventory::retain(&published);
        let mut named = retained.clone();
        named.revision += 1;
        named.workspaces[0].tabs[0].panes[0].shell = Some("zsh".into());
        let mut renamed = retained.clone();
        renamed.workspaces[0].name = "Play".into();
        let mut split = retained.clone();
        split.workspaces[0].tabs[0].panes[1].axis = Axis::Right;

        assert!(retained.same_structure(&named));
        assert!(!retained.same_structure(&renamed));
        assert!(!retained.same_structure(&split));
    }
}
