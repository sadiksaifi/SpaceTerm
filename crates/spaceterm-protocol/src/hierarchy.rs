//! Workspace Inventory, mutations and bounded operation outcomes.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub type Id = [u8; 32];

pub const MAX_WORKSPACES: usize = 64;
pub const MAX_TABS: usize = 128;
pub const MAX_PANES: usize = 128;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Inventory {
    /// Live owning-server Git facts. Excluded from retained Organization.
    pub repositories: Option<crate::repository::Snapshot>,
    pub revision: u64,
    pub workspaces: Vec<Workspace>,
    /// Live facts accompany publication only; retained organization never supplies authority.
    pub close: Option<CloseInventory>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CloseTarget {
    Pane {
        workspace: Id,
        tab: Id,
        pane: Id,
    },
    Tab {
        workspace: Id,
        tab: Id,
    },
    KillWorkspace {
        workspace: Id,
    },
    /// One surviving Tab identifies the server-owned Worktree membership to close.
    Worktree {
        workspace: Id,
        tab: Id,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfirmedPane {
    pub id: Id,
    pub session: Option<Id>,
}

/// A public description of one control grant, never an input capability.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfirmedControl {
    pub pane: Id,
    pub session: Option<Id>,
    pub owner: Id,
    /// Scoped to this Pane and Terminal Session. A transfer changes it even if ownership returns.
    pub epoch: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloseInventory {
    pub boot: Id,
    pub sessions: Vec<ConfirmedPane>,
    /// Each Pane's control grant, which fences confirmation-free closes.
    pub controls: Vec<ConfirmedControl>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloseConfirmation {
    pub boot: Id,
    pub panes: Vec<ConfirmedPane>,
    pub removes_tab: bool,
    pub removes_workspace: bool,
    /// `None` carries explicit consent. A nonempty list fences an idle close to these control
    /// grants. An empty list marks an idle close independent of control, which uses the
    /// `CloseIfIdle` work request.
    pub own_controls: Option<Vec<ConfirmedControl>>,
}

impl CloseConfirmation {
    pub fn is_close_if_idle(&self) -> bool {
        self.own_controls.as_ref().is_some_and(Vec::is_empty)
    }

    /// Whether both name the same boot, Panes, Terminal Sessions and removals.
    pub fn same_targets(&self, other: &Self) -> bool {
        self.boot == other.boot
            && self.panes == other.panes
            && self.removes_tab == other.removes_tab
            && self.removes_workspace == other.removes_workspace
    }
}

/// One Pane with the Tab and Workspace that hold it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    pub workspace: Id,
    pub tab: Id,
    pub pane: Id,
}

impl Inventory {
    /// Whether the latest Machine observation verified running work in the exact Sessions that
    /// this close would end. `None` means the inventory lacks a matching observation.
    /// Core checks again when it admits an idle-only close. Workspace removal still needs consent.
    pub fn close_would_end_work(&self, target: CloseTarget) -> Option<bool> {
        let confirmed = self.close_confirmation(target)?;
        let mut complete = true;
        for pane in confirmed.panes {
            let activity = self
                .workspaces
                .iter()
                .flat_map(|workspace| &workspace.tabs)
                .flat_map(|tab| &tab.panes)
                .find(|entry| entry.id == pane.id)
                .and_then(|entry| entry.activity)
                .filter(|activity| activity.session == pane.session);
            match activity {
                Some(activity) if activity.needs_close_review => return Some(true),
                None => complete = false,
                _ => {}
            }
        }
        complete.then_some(false)
    }

    /// Where created work `id` sits, as a Reveal shows it: a Workspace shows its Root Tab's Root
    /// Pane, and a Tab its Root Pane.
    pub fn placement(&self, id: Id) -> Option<Placement> {
        self.workspaces.iter().find_map(|w| {
            let at = |tab: &Tab, pane: Id| Placement {
                workspace: w.id,
                tab: tab.id,
                pane,
            };
            if w.id == id {
                let tab = w.tabs.iter().find(|t| t.id == w.root_tab)?;
                return Some(at(tab, tab.root_pane));
            }
            w.tabs.iter().find_map(|t| {
                if t.id == id {
                    return Some(at(t, t.root_pane));
                }
                t.panes.iter().any(|p| p.id == id).then(|| at(t, id))
            })
        })
    }
    pub fn close_confirmation(&self, target: CloseTarget) -> Option<CloseConfirmation> {
        let live = self.close.as_ref()?;
        let ids = self.close_panes(target)?;
        let workspace = match target {
            CloseTarget::Pane { workspace, .. }
            | CloseTarget::Tab { workspace, .. }
            | CloseTarget::KillWorkspace { workspace }
            | CloseTarget::Worktree { workspace, .. } => workspace,
        };
        let workspace = self.workspaces.iter().find(|w| w.id == workspace)?;
        // Sibling removal can change the consequences without changing any target Pane.
        let removes_tab = match target {
            CloseTarget::Pane { tab, .. } => {
                workspace.tabs.iter().find(|t| t.id == tab)?.panes.len() == 1
            }
            CloseTarget::Tab { .. }
            | CloseTarget::KillWorkspace { .. }
            | CloseTarget::Worktree { .. } => true,
        };
        let removes_workspace =
            ids.len() == workspace.tabs.iter().map(|t| t.panes.len()).sum::<usize>();
        let panes = ids
            .into_iter()
            .map(|id| live.sessions.iter().find(|p| p.id == id).cloned())
            .collect::<Option<Vec<_>>>()?;
        Some(CloseConfirmation {
            own_controls: None,
            boot: live.boot,
            panes,
            removes_tab,
            removes_workspace,
        })
    }

    pub fn close_panes(&self, target: CloseTarget) -> Option<Vec<Id>> {
        let workspace = match target {
            CloseTarget::Pane { workspace, .. }
            | CloseTarget::Tab { workspace, .. }
            | CloseTarget::KillWorkspace { workspace }
            | CloseTarget::Worktree { workspace, .. } => workspace,
        };
        let w = self.workspaces.iter().find(|w| w.id == workspace)?;
        match target {
            CloseTarget::Worktree { tab, .. } => {
                let anchor = w.tabs.iter().find(|t| t.id == tab)?;
                anchor.worktree.as_ref()?;
                Some(
                    w.tabs
                        .iter()
                        .filter(|t| {
                            t.worktree == anchor.worktree
                                && t.worktree_repository == anchor.worktree_repository
                        })
                        .flat_map(|t| &t.panes)
                        .map(|p| p.id)
                        .collect(),
                )
            }
            CloseTarget::KillWorkspace { .. } => {
                Some(w.tabs.iter().flat_map(|t| &t.panes).map(|p| p.id).collect())
            }
            CloseTarget::Tab { tab, .. } => Some(
                w.tabs
                    .iter()
                    .find(|t| t.id == tab)?
                    .panes
                    .iter()
                    .map(|p| p.id)
                    .collect(),
            ),
            CloseTarget::Pane { tab, pane, .. } => w
                .tabs
                .iter()
                .find(|t| t.id == tab)?
                .panes
                .iter()
                .any(|p| p.id == pane)
                .then_some(vec![pane]),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    pub id: Id,
    pub name: String,
    pub name_version: u64,
    pub pin_version: u64,
    pub pinned_directory: Option<PathBuf>,
    pub root_tab: Id,
    pub tabs: Vec<Tab>,
}
impl Workspace {
    /// Moves a Tab to its zero-based final position in this Workspace, keeping its identity,
    /// Panes, and Worktree membership.
    pub fn reorder_tab(&mut self, tab: Id, index: u16) -> Result<(), Outcome> {
        let Some(from) = self.tabs.iter().position(|candidate| candidate.id == tab) else {
            return Err(Outcome::Missing);
        };
        let to = usize::from(index);
        if to >= self.tabs.len() {
            return Err(Outcome::Invalid);
        }
        if from != to {
            let tab = self.tabs.remove(from);
            self.tabs.insert(to, tab);
        }
        Ok(())
    }

    /// The Main Worktree of the repository this Workspace is in.
    pub fn repository(&self) -> Option<&Path> {
        self.placed_tab()?.worktree_repository.as_deref()
    }

    /// The Tab whose saved membership names this Workspace's repository: the Root Tab when it
    /// has a Worktree, else the first Tab that does.
    pub fn placed_tab(&self) -> Option<&Tab> {
        let placed = |tab: &&Tab| tab.worktree.is_some() && tab.worktree_repository.is_some();
        self.tabs
            .iter()
            .find(|tab| tab.id == self.root_tab)
            .filter(placed)
            .or_else(|| self.tabs.iter().find(placed))
    }
}

impl Workspace {
    /// Moves `pane` out of `tab`, with its Terminal Session, into a new last Tab `id` in the same
    /// Worktree, where it is the Root Pane.
    ///
    /// A refusal changes nothing: `Outcome::Missing` when `tab` does not hold `pane`, and
    /// `Outcome::Invalid` when `pane` is the only Pane of `tab`.
    pub fn move_pane_to_new_tab(&mut self, tab: Id, pane: Id, id: Id) -> Result<(), Outcome> {
        let source = self
            .tabs
            .iter()
            .find(|t| t.id == tab)
            .ok_or(Outcome::Missing)?;
        let (worktree, worktree_repository) =
            (source.worktree.clone(), source.worktree_repository.clone());
        let moved = self.take_pane(tab, pane)?;
        self.tabs.push(Tab {
            worktree_membership: WorktreeMembership::Following,
            worktree,
            worktree_repository,
            id,
            root_pane: pane,
            panes: vec![moved],
        });
        Ok(())
    }

    /// Moves `pane` out of `tab`, with its Terminal Session, to the `side` of Pane `beside` in
    /// another Tab `into`.
    ///
    /// A refusal changes nothing: `Outcome::Missing` when either Tab does not hold its Pane, and
    /// `Outcome::Invalid` when `into` is `tab` or `pane` is the only Pane of `tab`.
    pub fn move_pane_beside(
        &mut self,
        tab: Id,
        pane: Id,
        into: Id,
        beside: Id,
        side: Side,
    ) -> Result<(), Outcome> {
        let target = self.destination(tab, into, beside)?;
        let mut moved = self.take_pane(tab, pane)?;
        moved.source = Some(beside);
        moved.side = side;
        self.tabs[target].panes.push(moved);
        Ok(())
    }

    /// Moves every Pane of `tab`, with their Terminal Sessions, to the `side` of Pane `beside` in
    /// another Tab `into`, and removes `tab`.
    ///
    /// The Panes keep the arrangement their Tab gave them, with its Root Pane beside `beside`.
    /// Merging the Root Tab promotes the Tab that follows it. A refusal changes nothing:
    /// `Outcome::Missing` when either Tab is gone or `into` does not hold `beside`, and
    /// `Outcome::Invalid` when `into` is `tab`.
    pub fn merge_tab(&mut self, tab: Id, into: Id, beside: Id, side: Side) -> Result<(), Outcome> {
        self.destination(tab, into, beside)?;
        let index = self
            .tabs
            .iter()
            .position(|t| t.id == tab)
            .ok_or(Outcome::Missing)?;
        let merged = self.tabs.remove(index);
        let target = self
            .tabs
            .iter_mut()
            .find(|t| t.id == into)
            .ok_or(Outcome::Missing)?;
        for mut pane in merged.panes {
            if pane.id == merged.root_pane {
                pane.source = Some(beside);
                pane.side = side;
            } else {
                // A Pane whose own source is gone took its place beside the Root Pane it now
                // follows into the other Tab.
                pane.source.get_or_insert(merged.root_pane);
            }
            target.panes.push(pane);
        }
        self.promote_root_tab();
        Ok(())
    }

    /// The index of Tab `into`, which must be another Tab than `tab` and hold Pane `beside`.
    fn destination(&self, tab: Id, into: Id, beside: Id) -> Result<usize, Outcome> {
        if tab == into {
            return Err(Outcome::Invalid);
        }
        self.tabs
            .iter()
            .position(|t| t.id == into && t.panes.iter().any(|p| p.id == beside))
            .ok_or(Outcome::Missing)
    }

    /// Removes one of several Panes from `tab`. The removed Pane starts no default layout of its
    /// own, the Panes split from it take their places beside the Root Pane, and removing the Root
    /// Pane promotes the Pane that follows it.
    fn take_pane(&mut self, tab: Id, pane: Id) -> Result<Pane, Outcome> {
        let source = self
            .tabs
            .iter_mut()
            .find(|t| t.id == tab)
            .ok_or(Outcome::Missing)?;
        let index = source
            .panes
            .iter()
            .position(|p| p.id == pane)
            .ok_or(Outcome::Missing)?;
        if source.panes.len() == 1 {
            return Err(Outcome::Invalid);
        }
        let mut moved = source.panes.remove(index);
        moved.source = None;
        moved.side = Side::Right;
        for p in &mut source.panes {
            if p.source == Some(pane) {
                p.source = None;
            }
        }
        if source.root_pane == pane {
            source.root_pane = source.panes[0].id;
        }
        Ok(moved)
    }

    /// Makes the first Tab the Root Tab once the Root Tab is gone. It stays in its Worktree.
    pub fn promote_root_tab(&mut self) {
        if let Some(t) = self.tabs.first_mut()
            && t.id != self.root_tab
        {
            self.root_tab = t.id;
        }
    }
}
/// Whether a Tab's shell may take the Tab, and its Workspace, to a different Worktree.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorktreeMembership {
    /// The Tab follows its shell while it leads its Workspace: as the only Tab, or as the Root
    /// Tab of a Workspace in no repository.
    #[default]
    Following,
    /// The Tab stays in its Worktree. A `cd` as its Workspace's only Tab ends this.
    Opened,
    /// Directory repair kept the Tab's Worktree. Leaving every repository as its Workspace's
    /// only Tab ends this. A change of Pinned Directory moves the Tab and keeps this.
    Repaired,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tab {
    pub worktree_membership: WorktreeMembership,
    /// Saved membership, retained even when Git no longer lists the directory.
    pub worktree: Option<PathBuf>,
    /// Main Worktree discovery locator for this membership, revalidated after each boot.
    pub worktree_repository: Option<PathBuf>,
    pub id: Id,
    pub root_pane: Id,
    pub panes: Vec<Pane>,
}
/// How many observer Client Names accompany a Pane's live inventory.
pub const MAX_OBSERVER_NAMES: usize = 3;

/// Distinct clients with open observer channels. Normal desktop presentation is not observation.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneObservers {
    pub names: Vec<crate::pane::control::ClientName>,
    /// Clients beyond the names above, independent of their number of channels.
    pub others: u32,
}

/// Who owned the PTY foreground when the Machine last observed the Terminal Session.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Foreground {
    #[default]
    Unknown,
    Shell,
    Program,
}

/// The live Machine observation used to prepare an exact-session close.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneActivity {
    pub session: Option<Id>,
    pub foreground: Foreground,
    /// Core also checks submitted input and other work that closing the Session would end.
    pub needs_close_review: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pane {
    pub id: Id,
    /// The Pane this Pane was placed beside in the default layout, which every client without a
    /// saved arrangement derives.
    pub source: Option<Id>,
    /// The side of `source` this Pane takes in the default layout.
    pub side: Side,
    pub starting_directory: PathBuf,
    /// Live terminal directory presentation. Never retained as restoration authority.
    pub current_directory: Option<PathBuf>,
    /// Protected Repository Source Directory from the owning Terminal Session, never retained.
    pub repository_directory: Option<PathBuf>,
    /// Current presentation for every authorized client, independent of Pane control or visibility.
    pub presentation: Option<Box<crate::pane::metadata::Snapshot>>,
    /// Live observation presence, excluded from retained Organization.
    pub observers: PaneObservers,
    /// Published in the work 1.6 inventory frame. Older peers and retained inventory omit it.
    #[serde(skip)]
    pub activity: Option<PaneActivity>,
    pub confirmed_directory: Option<PathBuf>,
    /// The creator reserves control even before measured first use starts a shell.
    pub creator: Option<Id>,
}
impl Pane {
    pub fn repository_directory(&self) -> &Path {
        self.repository_directory
            .as_deref()
            .or(self.confirmed_directory.as_deref())
            .unwrap_or(&self.starting_directory)
    }

    pub fn presented_directory(&self) -> &Path {
        self.current_directory
            .as_deref()
            .or(self.confirmed_directory.as_deref())
            .unwrap_or(&self.starting_directory)
    }
}
/// How a Split places its second child after its first: to the right, or below.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Axis {
    Right,
    Down,
}
/// The side of another Pane a Pane takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    Right,
    Down,
    Left,
    Up,
}
impl Side {
    /// The axis of the Split that places a Pane on this side.
    pub const fn axis(self) -> Axis {
        match self {
            Self::Right | Self::Left => Axis::Right,
            Self::Down | Self::Up => Axis::Down,
        }
    }
    /// Whether the Pane comes first in that Split, on the left or on top.
    pub const fn leads(self) -> bool {
        matches!(self, Self::Left | Self::Up)
    }
}
impl From<Axis> for Side {
    fn from(axis: Axis) -> Self {
        match axis {
            Axis::Right => Self::Right,
            Axis::Down => Self::Down,
        }
    }
}
/// Where a moved Pane goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PaneDestination {
    /// A new last Tab in the source Tab's Worktree, where the Pane is the Root Pane.
    NewTab,
    /// The `side` of Pane `pane` in another Tab `tab` of the same Workspace.
    Beside { tab: Id, pane: Id, side: Side },
}
/// Who controls work a mutation creates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CreateControl {
    /// The creating client controls it.
    Creator,
    /// The first client to select it acquires it.
    Unclaimed,
    /// The client that controls `pane` at commit controls it, and receives `Effect::Reveal`.
    /// A Pane with no controller falls back to `Unclaimed`.
    OriginController { pane: Id },
}

impl CreateControl {
    /// The client that controls work `device` creates under this choice. `OriginController`
    /// reads the origin Pane's control grant in `inventory`, which carries the live grants at
    /// commit.
    pub fn creator(&self, device: Id, inventory: &Inventory) -> Option<Id> {
        match self {
            Self::Creator => Some(device),
            Self::Unclaimed => None,
            Self::OriginController { pane } => inventory
                .close
                .as_ref()?
                .controls
                .iter()
                .find(|control| control.pane == *pane)
                .map(|control| control.owner),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mutation {
    /// End the Sessions on this running server without removing their Panes.
    EndAllSessions {
        confirmation: CloseInventory,
    },
    Worktree(crate::worktree::Operation),
    Close {
        target: CloseTarget,
        confirmation: CloseConfirmation,
    },
    CreateWorkspace {
        name: String,
        /// Pins the new Workspace here, and its first Pane starts here.
        directory: Option<PathBuf>,
        control: CreateControl,
    },
    CreateTab {
        workspace: Id,
        source: Option<Id>,
        control: CreateControl,
    },
    SplitPane {
        workspace: Id,
        tab: Id,
        source: Id,
        axis: Axis,
        control: CreateControl,
    },
    /// Move one of a Tab's several Panes, with its Terminal Session, to `destination` in the
    /// same Workspace.
    MovePane {
        workspace: Id,
        tab: Id,
        pane: Id,
        destination: PaneDestination,
    },
    /// Move every Pane of `tab`, with their Terminal Sessions, to the `side` of Pane `beside` in
    /// Tab `into`, and remove `tab`.
    MergeTab {
        workspace: Id,
        tab: Id,
        into: Id,
        beside: Id,
        side: Side,
    },
    RenameWorkspace {
        workspace: Id,
        version: u64,
        name: String,
    },
    PinWorkspace {
        workspace: Id,
        version: u64,
        directory: Option<PathBuf>,
    },
    /// Move a Tab to its zero-based final position in the Workspace. Requires work 1.6.
    ReorderTab {
        workspace: Id,
        tab: Id,
        index: u16,
    },
}
impl Mutation {
    /// Requests a Pane or Tab close without confirmation, regardless of control ownership.
    /// Core rechecks running work in the exact reviewed Sessions. Requires work 1.5.
    pub fn close_if_idle(target: CloseTarget, mut confirmation: CloseConfirmation) -> Self {
        confirmation.own_controls = Some(Vec::new());
        Self::Close {
            target,
            confirmation,
        }
    }

    pub fn is_close_if_idle(&self) -> bool {
        matches!(self, Self::Close { confirmation, .. } if confirmation.is_close_if_idle())
    }

    /// The work capability a peer needs before accepting this edit.
    pub fn required_capability(&self) -> Option<crate::protocol::capability::Capability> {
        use crate::protocol::capability::Capability;
        match self {
            Self::ReorderTab { .. } => Some(Capability::ReorderTab),
            _ if self.is_close_if_idle() => Some(Capability::CloseIfIdle),
            _ => None,
        }
    }

    /// Who controls what this mutation creates, for those that create work.
    pub fn create_control(&self) -> Option<CreateControl> {
        match self {
            Self::CreateWorkspace { control, .. }
            | Self::CreateTab { control, .. }
            | Self::SplitPane { control, .. }
            | Self::Worktree(
                crate::worktree::Operation::OpenTab { control, .. }
                | crate::worktree::Operation::Create { control, .. },
            ) => Some(*control),
            _ => None,
        }
    }
    pub fn is_bounded(&self) -> bool {
        match self {
            Self::EndAllSessions { confirmation } => {
                confirmation.sessions.len() <= MAX_PANES && confirmation.controls.len() <= MAX_PANES
            }
            Self::Worktree(operation) => operation.is_bounded(),
            Self::Close { confirmation, .. } => {
                !confirmation.panes.is_empty()
                    && confirmation.panes.len() <= MAX_PANES
                    && confirmation
                        .own_controls
                        .as_ref()
                        .is_none_or(|c| c.len() <= MAX_PANES)
            }
            Self::CreateWorkspace {
                name, directory, ..
            } => name.len() <= 256 && directory.as_deref().is_none_or(valid_path),
            Self::RenameWorkspace { name, .. } => name.len() <= 256,
            Self::PinWorkspace {
                directory: Some(path),
                ..
            } => valid_path(path),
            _ => true,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome {
    Pending,
    WorktreeFailed(crate::worktree::Error),
    Accepted { target: Id },
    Conflict,
    Missing,
    Invalid,
    Limit,
    StaleConfirmation,
}
impl Outcome {
    /// Whether the client that asked must present this answer itself. An accepted change
    /// reaches every client through the inventory, and a pending one has no answer yet.
    pub const fn needs_presentation(&self) -> bool {
        !matches!(self, Self::Pending | Self::Accepted { .. })
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetainedOutcome {
    pub sequence: u64,
    pub mutation: Mutation,
    pub outcome: Outcome,
}
/// Whether a path is absolute, fits a wire path and contains no NUL byte.
pub fn valid_path(path: &Path) -> bool {
    path.is_absolute()
        && path.as_os_str().len() <= crate::protocol::limits::MAX_PATH_BYTES
        && !path.as_os_str().as_encoded_bytes().contains(&0)
}
