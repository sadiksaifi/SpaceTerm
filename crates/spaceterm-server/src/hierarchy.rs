//! Authoritative shared membership and bounded, durable mutation outcomes.

use crate::{ServerError, pane::Refusal};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub type Id = [u8; 32];
pub const MAX_WORKSPACES: usize = 64;
pub const MAX_TABS: usize = 128;
pub const MAX_PANES: usize = 128;
const MAX_OUTCOMES: usize = 32;
/// Editing devices the server keeps records for at once. See [`State::apply`].
const MAX_DEVICES: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Inventory {
    /// Live owning-server Git facts. Excluded from retained Organization.
    #[serde(default)]
    pub repositories: Option<crate::repository::Snapshot>,
    pub revision: u64,
    pub workspaces: Vec<Workspace>,
    /// Live facts accompany publication only; retained organization never supplies authority.
    #[serde(default)]
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
    Workspace {
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
    /// Only a confirmation-free own close binds control in addition to exact membership.
    pub own_controls: Option<Vec<ConfirmedControl>>,
}

impl CloseConfirmation {
    fn same_targets(&self, other: &Self) -> bool {
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
            | CloseTarget::Workspace { workspace }
            | CloseTarget::Worktree { workspace, .. } => workspace,
        };
        let workspace = self.workspaces.iter().find(|w| w.id == workspace)?;
        // Sibling removal can change the consequences without changing any target Pane.
        let removes_tab = match target {
            CloseTarget::Pane { tab, .. } => {
                workspace.tabs.iter().find(|t| t.id == tab)?.panes.len() == 1
            }
            CloseTarget::Tab { .. }
            | CloseTarget::Workspace { .. }
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

    fn close_panes(&self, target: CloseTarget) -> Option<Vec<Id>> {
        let workspace = match target {
            CloseTarget::Pane { workspace, .. }
            | CloseTarget::Tab { workspace, .. }
            | CloseTarget::Workspace { workspace }
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
            CloseTarget::Workspace { .. } => {
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
/// Whether directory changes may move a Tab to a different Worktree, and why not.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorktreeMembership {
    /// Directory reads assign membership under the Root Tab rule.
    #[default]
    Following,
    /// The Tab opened in its Worktree. Promotion to Root Tab ends this.
    Opened,
    /// Directory repair kept the Tab's Worktree. Promotion to Root Tab keeps this.
    Repaired,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tab {
    /// An unpinned Root Tab whose Root Pane reads outside any repository returns to Following.
    #[serde(default)]
    pub worktree_membership: WorktreeMembership,
    /// Saved membership, retained even when Git no longer lists the directory.
    #[serde(default)]
    pub worktree: Option<PathBuf>,
    /// Main Worktree discovery locator for this membership, revalidated after each boot.
    #[serde(default)]
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pane {
    pub id: Id,
    pub source: Option<Id>,
    pub axis: Axis,
    pub starting_directory: PathBuf,
    /// Live terminal directory presentation. Never retained as restoration authority.
    pub current_directory: Option<PathBuf>,
    /// Protected Repository Source Directory from the owning Terminal Session, never retained.
    #[serde(default)]
    pub repository_directory: Option<PathBuf>,
    /// Current presentation for every authorized client, independent of Pane control or visibility.
    pub presentation: Option<Box<crate::pane::metadata::Snapshot>>,
    /// Live observation presence, excluded from retained Organization.
    pub observers: PaneObservers,
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Axis {
    Right,
    Down,
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
}
impl Mutation {
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
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct DeviceOutcomes {
    device: Id,
    retired: u64,
    pending: Vec<RetainedOutcome>,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct WorkspaceName {
    workspace: Id,
    custom: Option<String>,
    automatic_base: String,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    home_directory: Option<PathBuf>,
    names: Vec<WorkspaceName>,
    #[serde(default)]
    pub worktree_settings: Option<crate::worktree::Settings>,
    pub inventory: Inventory,
    /// Ordered from the least to the most recently editing device.
    devices: Vec<DeviceOutcomes>,
    /// The highest sequence of any device record the server replaced. A device without a record
    /// continues above it, so a replaced device's new edits never reuse a sequence. Its
    /// unresolved Original Operation keeps its own sequence, which [`State::apply`] accepts.
    #[serde(default)]
    sequence_floor: u64,
    #[serde(default)]
    creator_boot: Option<Id>,
    /// Durable cleanup intent names original Sessions, never future replacements.
    session_ends: Option<CloseInventory>,
}
impl State {
    pub fn empty() -> Self {
        Self {
            home_directory: None,
            names: Vec::new(),
            worktree_settings: None,
            inventory: Inventory {
                repositories: None,
                revision: 0,
                workspaces: Vec::new(),
                close: None,
            },
            devices: Vec::new(),
            sequence_floor: 0,
            creator_boot: None,
            session_ends: None,
        }
    }
    pub fn begin_boot(&mut self, boot: Id) {
        if self.creator_boot != Some(boot) {
            self.session_ends = None;
            for w in &mut self.inventory.workspaces {
                for t in &mut w.tabs {
                    for p in &mut t.panes {
                        p.creator = None;
                        p.current_directory = None;
                        p.repository_directory = None;
                        p.presentation = None;
                        p.observers = PaneObservers::default();
                    }
                }
            }
            self.creator_boot = Some(boot);
        }
    }
    /// First-Workspace seeding has no shared-edit operation identity. It must not retire a
    /// sequence a concurrent client edit has already reserved in its durable journal.
    pub fn ensure_first_workspace(&mut self, device: Id, directory: &Path) -> Result<(), Refusal> {
        if self.inventory.workspaces.is_empty() {
            self.edit(
                device,
                &Mutation::CreateWorkspace {
                    name: "Development".into(),
                    directory: None,
                    control: CreateControl::Creator,
                },
                directory,
                |_| true,
            )?;
        }
        Ok(())
    }
    pub fn from_organization(org: &crate::organization::Organization) -> Self {
        org.shared.as_deref().cloned().unwrap_or_else(|| Self {
            home_directory: Some(org.starting_directory.clone()),
            names: vec![WorkspaceName {
                workspace: org.workspace,
                custom: (org.name != "Default").then(|| org.name.clone()),
                automatic_base: "Default".into(),
            }],
            worktree_settings: None,
            inventory: Inventory {
                repositories: None,
                revision: 0,
                close: None,
                workspaces: vec![Workspace {
                    id: org.workspace,
                    name: org.name.clone(),
                    name_version: 0,
                    pin_version: 0,
                    pinned_directory: org.pinned_directory.clone(),
                    root_tab: org.tab,
                    tabs: vec![Tab {
                        worktree: None,
                        worktree_membership: WorktreeMembership::Following,
                        worktree_repository: None,
                        id: org.tab,
                        root_pane: org.pane,
                        panes: vec![Pane {
                            id: org.pane,
                            source: None,
                            axis: Axis::Right,
                            starting_directory: org.starting_directory.clone(),
                            current_directory: None,
                            repository_directory: None,
                            presentation: None,
                            observers: Default::default(),
                            confirmed_directory: org.confirmed_directory.clone(),
                            creator: None,
                        }],
                    }],
                }],
            },
            devices: Vec::new(),
            sequence_floor: 0,
            creator_boot: None,
            session_ends: None,
        })
    }
    pub fn envelope(
        &self,
        directory: &Path,
    ) -> Result<crate::organization::Organization, ServerError> {
        // The envelope describes the first Workspace. Shared organization is authoritative,
        // including when its inventory is empty.
        let mut org = crate::organization::Organization::first(directory.to_owned())?;
        if let Some(w) = self.inventory.workspaces.first() {
            let t = w
                .tabs
                .iter()
                .find(|t| t.id == w.root_tab)
                .ok_or(ServerError::UnsafeStorage)?;
            let p = t
                .panes
                .iter()
                .find(|p| p.id == t.root_pane)
                .ok_or(ServerError::UnsafeStorage)?;
            org.workspace = w.id;
            org.tab = t.id;
            org.pane = p.id;
            org.name.clone_from(&w.name);
            org.pinned_directory.clone_from(&w.pinned_directory);
            org.starting_directory.clone_from(&p.starting_directory);
            org.confirmed_directory.clone_from(&p.confirmed_directory);
        }
        let mut retained = self.clone();
        retained.inventory.repositories = None;
        retained.inventory.close = None;
        for pane in retained
            .inventory
            .workspaces
            .iter_mut()
            .flat_map(|workspace| &mut workspace.tabs)
            .flat_map(|tab| &mut tab.panes)
        {
            pane.current_directory = None;
            pane.repository_directory = None;
            pane.presentation = None;
            pane.observers = PaneObservers::default();
        }
        org.shared = Some(Box::new(retained));
        Ok(org)
    }
    pub fn outcome(&self, device: Id, sequence: u64) -> Option<&RetainedOutcome> {
        self.devices
            .iter()
            .find(|d| d.device == device)?
            .pending
            .iter()
            .find(|o| o.sequence == sequence)
    }
    pub fn session_ends(&self) -> Option<&CloseInventory> {
        self.session_ends.as_ref()
    }
    pub fn next_sequence(&self, device: Id) -> u64 {
        self.devices
            .iter()
            .find(|d| d.device == device)
            .map_or(self.sequence_floor + 1, |d| {
                d.pending.last().map_or(d.retired + 1, |o| o.sequence + 1)
            })
    }
    /// Forgets a revoked device's record and every outcome it retains. A revoked identity never
    /// connects again, so nothing would acknowledge them. Returns `false`, changing nothing, while
    /// the device's Worktree write is still in progress.
    pub fn forget(&mut self, device: Id) -> bool {
        if self
            .devices
            .iter()
            .any(|d| d.device == device && d.pending.iter().any(|o| o.outcome == Outcome::Pending))
        {
            return false;
        }
        self.devices.retain(|d| d.device != device);
        true
    }
    pub fn pending_worktrees(&self) -> Vec<(Id, RetainedOutcome)> {
        self.devices
            .iter()
            .flat_map(|d| {
                d.pending
                    .iter()
                    .filter(|o| o.outcome == Outcome::Pending)
                    .map(|o| (d.device, o.clone()))
            })
            .collect()
    }
    pub fn finish_worktree(
        &mut self,
        device: Id,
        sequence: u64,
        outcome: Outcome,
    ) -> Result<(), Refusal> {
        let record = self
            .devices
            .iter_mut()
            .find(|d| d.device == device)
            .and_then(|d| d.pending.iter_mut().find(|o| o.sequence == sequence))
            .ok_or(Refusal::Missing)?;
        if record.outcome != Outcome::Pending
            || !matches!(record.mutation, Mutation::Worktree(_))
            || outcome == Outcome::Pending
        {
            return Err(Refusal::Stale);
        }
        record.outcome = outcome;
        Ok(())
    }
    /// Opens a Tab in a Worktree whose Pane `creator` controls, or nobody until first selection.
    pub fn add_worktree_tab(
        &mut self,
        creator: Option<Id>,
        workspace: Id,
        root: PathBuf,
        path: PathBuf,
        starting_directory: PathBuf,
    ) -> Result<Id, Refusal> {
        if self.tab_count() >= MAX_TABS || self.pane_count() >= MAX_PANES {
            return Err(Refusal::Capacity);
        }
        let w = self
            .inventory
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace)
            .ok_or(Refusal::Missing)?;
        let id = crate::protocol::random().map_err(|_| Refusal::Failed)?;
        let pane = crate::protocol::random().map_err(|_| Refusal::Failed)?;
        w.tabs.push(Tab {
            id,
            root_pane: pane,
            worktree: Some(path.clone()),
            worktree_repository: Some(root),
            worktree_membership: WorktreeMembership::Opened,
            panes: vec![Pane {
                id: pane,
                source: None,
                axis: Axis::Right,
                starting_directory,
                current_directory: None,
                repository_directory: None,
                presentation: None,
                observers: Default::default(),
                confirmed_directory: None,
                creator,
            }],
        });
        self.inventory.revision = self.inventory.revision.saturating_add(1);
        Ok(id)
    }
    pub fn apply(
        &mut self,
        device: Id,
        sequence: u64,
        mutation: Mutation,
        directory: &Path,
        validate_pin: impl FnOnce(&Path) -> bool,
    ) -> Result<Outcome, Refusal> {
        if !mutation.is_bounded() {
            return Err(Refusal::Unsupported);
        }
        if let Some(old) = self.outcome(device, sequence) {
            return if old.mutation == mutation {
                Ok(old.outcome.clone())
            } else {
                Err(Refusal::Stale)
            };
        }
        // A device without a record has nothing applied that it has not acknowledged, and its
        // client retires an outcome before acknowledging it. So the device has never applied any
        // sequence it may still send, such as a replaced device's unresolved Original Operation.
        let expected = if self.devices.iter().any(|d| d.device == device) {
            sequence == self.next_sequence(device)
        } else {
            (1..=self.next_sequence(device)).contains(&sequence)
        };
        if !expected || sequence == u64::MAX {
            return Err(Refusal::Stale);
        }
        if self.devices.iter().map(|d| d.pending.len()).sum::<usize>() >= MAX_OUTCOMES {
            return Err(Refusal::Capacity);
        }
        let record = match self.devices.iter().position(|d| d.device == device) {
            Some(index) => self.devices.remove(index),
            None => {
                // A full table replaces the least recently editing device that has nothing left
                // to acknowledge, such as a reinstalled app's old identity. A device that still
                // retains an outcome keeps it under ADR 0037 until it acknowledges it or is
                // revoked.
                if self.devices.len() >= MAX_DEVICES {
                    let idle = self
                        .devices
                        .iter()
                        .position(|d| d.pending.is_empty())
                        .ok_or(Refusal::Capacity)?;
                    let replaced = self.devices.remove(idle);
                    self.sequence_floor = self.sequence_floor.max(replaced.retired);
                }
                DeviceOutcomes {
                    device,
                    retired: sequence - 1,
                    pending: Vec::new(),
                }
            }
        };
        self.devices.push(record);
        let outcome = self.edit(device, &mutation, directory, validate_pin)?;
        self.devices
            .last_mut()
            .ok_or(Refusal::Failed)?
            .pending
            .push(RetainedOutcome {
                sequence,
                mutation,
                outcome: outcome.clone(),
            });
        Ok(outcome)
    }
    pub fn acknowledge(&mut self, device: Id, sequence: u64) -> Result<(), Refusal> {
        // Acknowledgements are cumulative, because nothing confirms that one arrived: a later
        // one, or the last one repeated after a reconnect, retires every earlier outcome. An
        // outcome this server never recorded needs no retiring.
        let Some(d) = self.devices.iter_mut().find(|d| d.device == device) else {
            return Ok(());
        };
        let settled = d
            .pending
            .iter()
            .take_while(|o| o.sequence <= sequence)
            .count();
        if d.pending[..settled]
            .iter()
            .any(|o| o.outcome == Outcome::Pending)
        {
            return Err(Refusal::Stale);
        }
        if let Some(last) = d.pending.drain(..settled).next_back() {
            d.retired = d.retired.max(last.sequence);
        }
        Ok(())
    }
    fn edit(
        &mut self,
        device: Id,
        mutation: &Mutation,
        directory: &Path,
        validate_pin: impl FnOnce(&Path) -> bool,
    ) -> Result<Outcome, Refusal> {
        self.home_directory
            .get_or_insert_with(|| directory.to_owned());
        let new_id = || crate::protocol::random().map_err(|_| Refusal::Failed);
        let valid_name = |name: &str| {
            !name.trim().is_empty() && name.len() <= 256 && !name.chars().any(char::is_control)
        };
        let target = match mutation {
            Mutation::EndAllSessions { confirmation } => {
                let Some(live) = &self.inventory.close else {
                    return Ok(Outcome::StaleConfirmation);
                };
                // Control changes do not broaden an explicit End All Sessions approval, so only
                // the exact Sessions are compared.
                if live.boot != confirmation.boot || live.sessions != confirmation.sessions {
                    return Ok(Outcome::StaleConfirmation);
                }
                self.session_ends = Some(confirmation.clone());
                confirmation.boot
            }
            Mutation::Worktree(_) => return Ok(Outcome::Pending),
            Mutation::Close {
                target,
                confirmation,
            } => {
                let Some(ids) = self.inventory.close_panes(*target) else {
                    return Ok(Outcome::Missing);
                };
                if self
                    .inventory
                    .close_confirmation(*target)
                    .is_none_or(|current| !current.same_targets(confirmation))
                {
                    return Ok(Outcome::StaleConfirmation);
                }
                if let Some(reviewed) = &confirmation.own_controls {
                    let current = self.inventory.close.as_ref().map(|live| &live.controls);
                    if reviewed.len() != confirmation.panes.len()
                        || confirmation.panes.iter().any(|pane| {
                            reviewed
                                .iter()
                                .filter(|control| {
                                    control.pane == pane.id
                                        && control.session == pane.session
                                        && control.owner == device
                                        && current.is_some_and(|current| current.contains(control))
                                })
                                .count()
                                != 1
                        })
                    {
                        return Ok(Outcome::StaleConfirmation);
                    }
                }
                // Close Tabs keeps an existing Tab outside the Worktree; it never starts one.
                if matches!(target, CloseTarget::Worktree { .. }) && confirmation.removes_workspace
                {
                    return Ok(Outcome::Invalid);
                }
                for id in ids {
                    self.remove_pane(id);
                }
                match target {
                    CloseTarget::Pane { pane, .. } => *pane,
                    CloseTarget::Tab { tab, .. } => *tab,
                    CloseTarget::Workspace { workspace }
                    | CloseTarget::Worktree { workspace, .. } => *workspace,
                }
            }
            Mutation::CreateWorkspace {
                name,
                directory: pin,
                control,
            } => {
                let creator = control.creator(device, &self.inventory);
                if (!name.trim().is_empty() && !valid_name(name))
                    || pin.as_deref().is_some_and(|path| !validate_pin(path))
                {
                    return Ok(Outcome::Invalid);
                }
                if self.inventory.workspaces.len() >= MAX_WORKSPACES
                    || self.pane_count() >= MAX_PANES
                    || self.tab_count() >= MAX_TABS
                {
                    return Ok(Outcome::Limit);
                }
                let id = new_id()?;
                let tab = new_id()?;
                let pane = new_id()?;
                self.names.push(WorkspaceName {
                    workspace: id,
                    custom: (!name.trim().is_empty()).then(|| name.trim().to_owned()),
                    automatic_base: String::new(),
                });
                self.inventory.workspaces.push(Workspace {
                    id,
                    name: name.trim().to_owned(),
                    name_version: 0,
                    pin_version: 0,
                    pinned_directory: pin.clone(),
                    root_tab: tab,
                    tabs: vec![Tab {
                        worktree: None,
                        worktree_membership: WorktreeMembership::Following,
                        worktree_repository: None,
                        id: tab,
                        root_pane: pane,
                        panes: vec![Pane {
                            id: pane,
                            source: None,
                            axis: Axis::Right,
                            starting_directory: pin.clone().unwrap_or_else(|| directory.to_owned()),
                            current_directory: None,
                            repository_directory: None,
                            presentation: None,
                            observers: Default::default(),
                            confirmed_directory: None,
                            creator,
                        }],
                    }],
                });
                id
            }
            Mutation::CreateTab {
                workspace,
                source,
                control,
            } => {
                if self.tab_count() >= MAX_TABS || self.pane_count() >= MAX_PANES {
                    return Ok(Outcome::Limit);
                }
                let creator = control.creator(device, &self.inventory);
                let Some(w) = self
                    .inventory
                    .workspaces
                    .iter_mut()
                    .find(|w| w.id == *workspace)
                else {
                    return Ok(Outcome::Missing);
                };
                let source = match source {
                    Some(id) => w.tabs.iter().flat_map(|t| &t.panes).find(|p| p.id == *id),
                    None => w
                        .tabs
                        .iter()
                        .find(|t| t.id == w.root_tab)
                        .and_then(|t| t.panes.iter().find(|p| p.id == t.root_pane)),
                };
                let Some(source) = source else {
                    return Ok(Outcome::Missing);
                };
                let start = w.pinned_directory.clone().unwrap_or_else(|| {
                    source
                        .confirmed_directory
                        .as_ref()
                        .unwrap_or(&source.starting_directory)
                        .clone()
                });
                if !validate_pin(&start) {
                    return Ok(Outcome::Invalid);
                }
                let id = new_id()?;
                let pane = new_id()?;
                w.tabs.push(Tab {
                    worktree: None,
                    worktree_membership: WorktreeMembership::Following,
                    worktree_repository: None,
                    id,
                    root_pane: pane,
                    panes: vec![Pane {
                        id: pane,
                        source: None,
                        axis: Axis::Right,
                        starting_directory: start,
                        current_directory: None,
                        repository_directory: None,
                        presentation: None,
                        observers: Default::default(),
                        confirmed_directory: None,
                        creator,
                    }],
                });
                id
            }
            Mutation::SplitPane {
                workspace,
                tab,
                source,
                axis,
                control,
            } => {
                if self.pane_count() >= MAX_PANES {
                    return Ok(Outcome::Limit);
                }
                let creator = control.creator(device, &self.inventory);
                let Some(w) = self
                    .inventory
                    .workspaces
                    .iter_mut()
                    .find(|w| w.id == *workspace)
                else {
                    return Ok(Outcome::Missing);
                };
                let Some(t) = w.tabs.iter_mut().find(|t| t.id == *tab) else {
                    return Ok(Outcome::Missing);
                };
                let Some(p) = t.panes.iter().find(|p| p.id == *source) else {
                    return Ok(Outcome::Missing);
                };
                // Scoped launches have been physically selected and validated by the server.
                let start = if t.worktree.is_some() {
                    directory.to_owned()
                } else {
                    w.pinned_directory.clone().unwrap_or_else(|| {
                        p.confirmed_directory
                            .as_ref()
                            .unwrap_or(&p.starting_directory)
                            .clone()
                    })
                };
                if !validate_pin(&start) {
                    return Ok(Outcome::Invalid);
                }
                let id = new_id()?;
                t.panes.push(Pane {
                    id,
                    source: Some(*source),
                    axis: *axis,
                    starting_directory: start,
                    current_directory: None,
                    repository_directory: None,
                    presentation: None,
                    observers: Default::default(),
                    confirmed_directory: None,
                    creator,
                });
                id
            }
            Mutation::RenameWorkspace {
                workspace,
                version,
                name,
            } => {
                if !valid_name(name) {
                    return Ok(Outcome::Invalid);
                }
                let Some(w) = self
                    .inventory
                    .workspaces
                    .iter_mut()
                    .find(|w| w.id == *workspace)
                else {
                    return Ok(Outcome::Missing);
                };
                if *version != w.name_version {
                    return Ok(Outcome::Conflict);
                }
                let naming = self
                    .names
                    .iter_mut()
                    .find(|n| n.workspace == *workspace)
                    .ok_or(Refusal::Failed)?;
                naming.custom = Some(name.trim().to_owned());
                w.name_version += 1;
                *workspace
            }
            Mutation::PinWorkspace {
                workspace,
                version,
                directory,
            } => {
                let Some(w) = self
                    .inventory
                    .workspaces
                    .iter_mut()
                    .find(|w| w.id == *workspace)
                else {
                    return Ok(Outcome::Missing);
                };
                if *version != w.pin_version {
                    return Ok(Outcome::Conflict);
                }
                if let Some(path) = directory
                    && !validate_pin(path)
                {
                    return Ok(Outcome::Invalid);
                }
                w.pinned_directory.clone_from(directory);
                w.pin_version += 1;
                *workspace
            }
        };
        self.recalculate_names();
        self.inventory.revision = self
            .inventory
            .revision
            .checked_add(1)
            .ok_or(Refusal::Capacity)?;
        Ok(Outcome::Accepted { target })
    }
    fn tab_count(&self) -> usize {
        self.inventory.workspaces.iter().map(|w| w.tabs.len()).sum()
    }
    fn pane_count(&self) -> usize {
        self.inventory
            .workspaces
            .iter()
            .flat_map(|w| &w.tabs)
            .map(|t| t.panes.len())
            .sum()
    }
    pub fn pane(&self, id: Id) -> Option<(&Workspace, &Tab, &Pane)> {
        self.inventory.workspaces.iter().find_map(|w| {
            w.tabs
                .iter()
                .find_map(|t| t.panes.iter().find(|p| p.id == id).map(|p| (w, t, p)))
        })
    }
    /// Ordinary shell exit removes membership; Roots follow server creation order.
    pub fn remove_pane(&mut self, id: Id) {
        for w in &mut self.inventory.workspaces {
            for t in &mut w.tabs {
                t.panes.retain(|p| p.id != id);
                if let Some(p) = t.panes.first() {
                    t.root_pane = p.id;
                }
                // Keep a surviving creation source for defaults after its original was removed.
                let ids: Vec<_> = t.panes.iter().map(|p| p.id).collect();
                for p in &mut t.panes {
                    if p.source.is_some_and(|source| !ids.contains(&source)) {
                        p.source = None;
                    }
                }
            }
            w.tabs.retain(|t| !t.panes.is_empty());
            if let Some(t) = w.tabs.first_mut()
                && t.id != w.root_tab
            {
                // A promoted Root Tab follows its Root Pane, not the Worktree it opened in.
                if t.worktree_membership == WorktreeMembership::Opened {
                    t.worktree_membership = WorktreeMembership::Following;
                }
                w.root_tab = t.id;
            }
        }
        self.inventory.workspaces.retain(|w| !w.tabs.is_empty());
        self.names.retain(|name| {
            self.inventory
                .workspaces
                .iter()
                .any(|w| w.id == name.workspace)
        });
        self.recalculate_names();
        self.inventory.revision += 1;
    }
    pub fn repair_directory(&mut self, id: Id, path: PathBuf) -> Result<(), Refusal> {
        for workspace in &mut self.inventory.workspaces {
            for tab in &mut workspace.tabs {
                if let Some(pane) = tab.panes.iter_mut().find(|p| p.id == id) {
                    pane.confirmed_directory = Some(path);
                    tab.worktree_membership = WorktreeMembership::Repaired;
                    self.recalculate_names();
                    self.inventory.revision = self
                        .inventory
                        .revision
                        .checked_add(1)
                        .ok_or(Refusal::Capacity)?;
                    return Ok(());
                }
            }
        }
        Err(Refusal::Missing)
    }
    pub fn confirm_directory(&mut self, id: Id, path: PathBuf) {
        for w in &mut self.inventory.workspaces {
            for t in &mut w.tabs {
                if let Some(p) = t.panes.iter_mut().find(|p| p.id == id) {
                    if p.confirmed_directory.as_ref() != Some(&path) {
                        p.confirmed_directory = Some(path);
                        self.recalculate_names();
                        self.inventory.revision += 1;
                    }
                    return;
                }
            }
        }
    }
    /// Publish presentation in memory. The retained Organization excludes these facts.
    pub fn present_presentation(
        &mut self,
        id: Id,
        snapshot: Option<Box<crate::pane::metadata::Snapshot>>,
    ) {
        for workspace in &mut self.inventory.workspaces {
            for tab in &mut workspace.tabs {
                if let Some(pane) = tab.panes.iter_mut().find(|pane| pane.id == id) {
                    pane.presentation = snapshot;
                    return;
                }
            }
        }
    }
    /// Publish an OSC 7 directory without granting restoration or filesystem authority.
    pub fn present_directory(&mut self, id: Id, path: PathBuf) {
        for workspace in &mut self.inventory.workspaces {
            for tab in &mut workspace.tabs {
                if let Some(pane) = tab.panes.iter_mut().find(|pane| pane.id == id) {
                    if pane.presented_directory() != path {
                        pane.current_directory = Some(path);
                        self.recalculate_names();
                        self.inventory.revision += 1;
                    }
                    return;
                }
            }
        }
    }
    /// Publish the Terminal Session's protected Repository Source Directory independently of
    /// live OSC 7 display and confirmed restoration authority.
    pub fn present_repository_directory(&mut self, id: Id, path: PathBuf) {
        for workspace in &mut self.inventory.workspaces {
            for tab in &mut workspace.tabs {
                if let Some(pane) = tab.panes.iter_mut().find(|pane| pane.id == id) {
                    if pane.repository_directory() != path {
                        pane.repository_directory = Some(path);
                        self.inventory.revision += 1;
                    }
                    return;
                }
            }
        }
    }
    /// The allocation policy from domain/workspace_collection/naming.rs, at the local owner.
    fn recalculate_names(&mut self) {
        let mut occupied: std::collections::HashSet<String> =
            self.names.iter().filter_map(|n| n.custom.clone()).collect();
        let mut pending = Vec::new();
        for workspace in &mut self.inventory.workspaces {
            let Some(naming) = self.names.iter_mut().find(|n| n.workspace == workspace.id) else {
                continue;
            };
            if let Some(custom) = &naming.custom {
                if workspace.name != *custom {
                    workspace.name.clone_from(custom);
                }
                continue;
            }
            let root = workspace
                .tabs
                .iter()
                .find(|t| t.id == workspace.root_tab)
                .and_then(|t| t.panes.iter().find(|p| p.id == t.root_pane));
            let directory = workspace
                .pinned_directory
                .as_deref()
                .or_else(|| root.map(Pane::presented_directory));
            let mut base = match directory {
                Some(directory) if Some(directory) != self.home_directory.as_deref() => directory
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "/".into()),
                _ => "Default".into(),
            };
            // Leave room for the largest collision suffix within the protocol name bound.
            base.retain(|character| !character.is_control());
            while base.len() > 251 {
                base.pop();
            }
            if base.trim().is_empty() {
                base = "/".into();
            }
            // Reserve unchanged identities before allocating names for moving Workspaces.
            if naming.automatic_base == base && occupied.insert(workspace.name.clone()) {
                continue;
            }
            pending.push((workspace, base));
        }
        for (workspace, base) in pending {
            let Some(naming) = self.names.iter_mut().find(|n| n.workspace == workspace.id) else {
                continue;
            };
            let mut name = base.clone();
            let mut ordinal = 2;
            while !occupied.insert(name.clone()) {
                name = format!("{base} ({ordinal})");
                ordinal += 1;
            }
            if workspace.name != name {
                workspace.name = name;
                workspace.name_version += 1;
            }
            naming.automatic_base = base;
        }
    }
    pub(crate) fn validate(&self) -> Result<(), ServerError> {
        let invalid = || ServerError::UnsafeStorage;
        if let Some(ends) = &self.session_ends {
            let ids: std::collections::BTreeSet<_> = ends.sessions.iter().map(|p| p.id).collect();
            if Some(ends.boot) != self.creator_boot
                || ends.sessions.len() > MAX_PANES
                || ends.controls.len() > MAX_PANES
                || ids.len() != ends.sessions.len()
                || ends
                    .sessions
                    .iter()
                    .any(|p| p.id == [0; 32] || p.session == Some([0; 32]))
            {
                return Err(invalid());
            }
        }
        if self.names.len() != self.inventory.workspaces.len()
            || self
                .inventory
                .workspaces
                .iter()
                .any(|w| self.names.iter().filter(|n| n.workspace == w.id).count() != 1)
            || (!self.inventory.workspaces.is_empty() && self.home_directory.is_none())
            || self.names.iter().any(|n| {
                !self
                    .inventory
                    .workspaces
                    .iter()
                    .any(|w| w.id == n.workspace)
                    || n.custom.as_ref().is_some_and(|name| {
                        name.trim().is_empty()
                            || name.len() > 256
                            || name.chars().any(char::is_control)
                    })
            })
            || self
                .home_directory
                .as_deref()
                .is_some_and(|path| !valid_path(path))
            || self.inventory.workspaces.len() > MAX_WORKSPACES
            || self.tab_count() > MAX_TABS
            || self.pane_count() > MAX_PANES
            || self.devices.len() > MAX_DEVICES
            || self.sequence_floor == u64::MAX
            || self.devices.iter().map(|d| d.pending.len()).sum::<usize>() > MAX_OUTCOMES
        {
            return Err(invalid());
        }
        let mut ids = std::collections::BTreeSet::new();
        for w in &self.inventory.workspaces {
            if w.id == [0; 32]
                || !ids.insert(w.id)
                || w.name.trim().is_empty()
                || w.name.len() > 256
                || w.name.chars().any(char::is_control)
                || !w.tabs.iter().any(|t| t.id == w.root_tab)
                || w.pinned_directory.as_ref().is_some_and(|p| !valid_path(p))
            {
                return Err(invalid());
            }
            for t in &w.tabs {
                if t.id == [0; 32]
                    || t.worktree.as_ref().is_some_and(|p| !valid_path(p))
                    || t.worktree_repository
                        .as_ref()
                        .is_some_and(|p| !valid_path(p))
                    || (t.worktree_repository.is_some() && t.worktree.is_none())
                    || !ids.insert(t.id)
                    || !t.panes.iter().any(|p| p.id == t.root_pane)
                {
                    return Err(invalid());
                }
                let mut preceding = Vec::new();
                for p in &t.panes {
                    if p.id == [0; 32]
                        || !ids.insert(p.id)
                        || p.source.is_some_and(|id| !preceding.contains(&id))
                        || p.observers.names.len() > MAX_OBSERVER_NAMES
                        || (p.observers.names.is_empty() && p.observers.others != 0)
                        || p.presentation
                            .as_ref()
                            .is_some_and(|snapshot| snapshot.validate().is_err())
                        || !valid_path(&p.starting_directory)
                        || p.confirmed_directory
                            .as_ref()
                            .is_some_and(|p| !valid_path(p))
                    {
                        return Err(invalid());
                    }
                    preceding.push(p.id);
                }
            }
        }
        let mut devices = std::collections::BTreeSet::new();
        for d in &self.devices {
            if !devices.insert(d.device) || d.retired == u64::MAX {
                return Err(invalid());
            }
            for (index, o) in d.pending.iter().enumerate() {
                if (o.outcome == Outcome::Pending && !matches!(o.mutation, Mutation::Worktree(_)))
                    || !o.mutation.is_bounded()
                    || o.sequence
                        != d.retired
                            .checked_add(index as u64 + 1)
                            .ok_or_else(invalid)?
                    || o.sequence == u64::MAX
                {
                    return Err(invalid());
                }
            }
        }
        Ok(())
    }
}
fn valid_path(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    path.is_absolute()
        && path.as_os_str().len() <= 4096
        && !path.as_os_str().as_bytes().contains(&0)
}

#[cfg(feature = "client")]
#[derive(Clone, Serialize, Deserialize)]
pub struct ClientOperation {
    pub sequence: u64,
    pub mutation: Mutation,
}
/// How many retired outcomes the client keeps until it presents them. Beyond this, the oldest
/// is forgotten, and the client counts it so it can say that some went unseen.
#[cfg(feature = "client")]
pub const UNSEEN_OUTCOMES: usize = 32;
#[cfg(feature = "client")]
#[derive(Default, Serialize, Deserialize)]
struct JournalRecord {
    server: Id,
    client: Id,
    pending: Option<ClientOperation>,
    last: Option<RetainedOutcome>,
    /// Retired outcomes the client has not presented yet, oldest first. This is the client's
    /// own record; the server's acknowledgement retires an outcome independently.
    #[serde(default)]
    unseen: Vec<RetainedOutcome>,
    /// How many unseen outcomes the bound ever forgot, and how many of those the client has
    /// said went unseen. Both only grow, so recording a presentation again changes nothing.
    #[serde(default)]
    forgotten: u64,
    #[serde(default)]
    forgotten_presented: u64,
}
/// Pending shared edits live outside Settings and are fenced to their original authority.
#[cfg(feature = "client")]
pub struct ClientJournal {
    directory: crate::storage::Directory,
    _lock: crate::storage::Lock,
    record: JournalRecord,
}
#[cfg(feature = "client")]
impl ClientJournal {
    pub(crate) fn open(
        directory: crate::storage::Directory,
        server: Id,
        client: Id,
    ) -> Result<Self, ServerError> {
        let lock = directory.lock("operations.lock", true)?;
        Self::open_locked(directory, lock, server, client)
    }
    /// Opens the journal under `lock`, which the caller already holds on `operations.lock` of
    /// `directory`.
    pub(crate) fn open_locked(
        directory: crate::storage::Directory,
        lock: crate::storage::Lock,
        server: Id,
        client: Id,
    ) -> Result<Self, ServerError> {
        let record = directory
            .read_json::<JournalRecord>("operations.json")?
            .unwrap_or(JournalRecord {
                server,
                client,
                ..Default::default()
            });
        if record.server != server || record.client != client {
            return Err(ServerError::AuthenticationRefused);
        }
        Ok(Self {
            directory,
            _lock: lock,
            record,
        })
    }
    /// Opens the journal kept in `path`, with the same lock a client's journal takes, for a
    /// test that holds it the way a live edit loop does.
    #[cfg(feature = "test-support")]
    pub fn open_in(path: &Path, server: Id, client: Id) -> Result<Self, ServerError> {
        Self::open(crate::storage::Directory::open(path, true)?, server, client)
    }
    pub fn pending(&self) -> Option<&ClientOperation> {
        self.record.pending.as_ref()
    }
    /// How many operations the journal in `directory` still holds, read without its lock: the
    /// pending one and the last retired one, whose acknowledgement may not have arrived.
    pub fn held(directory: &crate::storage::Directory) -> Result<u32, ServerError> {
        Ok(directory
            .read_json::<JournalRecord>("operations.json")?
            .map_or(0, |record| {
                u32::from(record.pending.is_some()) + u32::from(record.last.is_some())
            }))
    }
    /// Drops the pending operation after the server said it recorded no such sequence, so it
    /// never applied it. Nothing dispatches it again.
    pub fn abandon(&mut self) -> Result<(), ServerError> {
        if self.record.pending.take().is_none() {
            return Ok(());
        }
        self.save()
    }
    /// Forgets the retired outcome of `sequence` once the server confirmed it forgot it too,
    /// for a client that presents each outcome itself when it arrives.
    pub fn settled(&mut self, sequence: u64) -> Result<(), ServerError> {
        if self
            .record
            .last
            .as_ref()
            .is_none_or(|last| last.sequence != sequence)
        {
            return Ok(());
        }
        self.record.last = None;
        self.record
            .unseen
            .retain(|unseen| unseen.sequence != sequence);
        self.save()
    }
    pub fn last(&self) -> Option<&RetainedOutcome> {
        self.record.last.as_ref()
    }
    /// Retired outcomes the client has not presented yet, oldest first.
    pub fn unseen(&self) -> &[RetainedOutcome] {
        &self.record.unseen
    }
    /// How many unseen outcomes the bound forgot in total, and how many of those the client has
    /// said went unseen.
    pub fn forgotten(&self) -> (u64, u64) {
        (self.record.forgotten, self.record.forgotten_presented)
    }
    /// Records that the client said the first `total` forgotten outcomes went unseen.
    pub fn forgotten_presented(&mut self, total: u64) -> Result<(), ServerError> {
        let total = total.min(self.record.forgotten);
        if total <= self.record.forgotten_presented {
            return Ok(());
        }
        self.record.forgotten_presented = total;
        self.save()
    }
    /// Records that the client presented the outcome of `sequence`, so it is never presented
    /// again. A sequence that is not unseen changes nothing.
    pub fn presented(&mut self, sequence: u64) -> Result<(), ServerError> {
        let before = self.record.unseen.len();
        self.record
            .unseen
            .retain(|unseen| unseen.sequence != sequence);
        if self.record.unseen.len() == before {
            return Ok(());
        }
        self.save()
    }
    pub fn submit(&mut self, sequence: u64, mutation: Mutation) -> Result<(), ServerError> {
        if self.record.pending.is_some() {
            return Err(ServerError::Unsupported);
        }
        self.record.pending = Some(ClientOperation { sequence, mutation });
        self.save()
    }
    /// Retires the pending operation with its validated outcome in one write. Acknowledge the
    /// outcome to the server only after this succeeds; [`Self::last`] keeps it to acknowledge
    /// again after a reconnect. An outcome the client must present also stays in
    /// [`Self::unseen`] until [`Self::presented`], whatever retires after it.
    pub fn retire(&mut self, result: RetainedOutcome) -> Result<(), ServerError> {
        let pending = self.record.pending.as_ref().ok_or(ServerError::Protocol)?;
        if pending.sequence != result.sequence || pending.mutation != result.mutation {
            return Err(ServerError::Protocol);
        }
        if result.outcome.needs_presentation() {
            if self.record.unseen.len() >= UNSEEN_OUTCOMES {
                self.record.unseen.remove(0);
                self.record.forgotten = self.record.forgotten.saturating_add(1);
            }
            self.record.unseen.push(result.clone());
        }
        self.record.last = Some(result);
        self.record.pending = None;
        self.save()
    }
    fn save(&mut self) -> Result<(), ServerError> {
        if let Err(error) = self.directory.write_json(c"operations.json", &self.record) {
            // Re-read and synchronize the actual retained result before this object is usable.
            self.record = self
                .directory
                .read_json("operations.json")?
                .ok_or(ServerError::StorageUnavailable)?;
            return Err(error);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_close_checks_requesting_owner_grant_and_complete_control_coverage() {
        let mut original = State::empty();
        original
            .ensure_first_workspace([1; 32], "/tmp".as_ref())
            .unwrap();
        let workspace = original.inventory.workspaces[0].id;
        let tab = original.inventory.workspaces[0].root_tab;
        let pane = original.inventory.workspaces[0].tabs[0].root_pane;
        let control = ConfirmedControl {
            pane,
            session: Some([7; 32]),
            owner: [1; 32],
            epoch: 3,
        };
        original.inventory.close = Some(CloseInventory {
            boot: [9; 32],
            sessions: vec![ConfirmedPane {
                id: pane,
                session: control.session,
            }],
            controls: vec![control.clone()],
        });
        let target = CloseTarget::Tab { workspace, tab };
        let explicit = original.inventory.close_confirmation(target).unwrap();
        let mut direct = explicit.clone();
        direct.own_controls = Some(vec![control.clone()]);
        for (device, fence, accepted) in [
            ([1; 32], direct.clone(), true),
            ([2; 32], direct.clone(), false),
            ([2; 32], explicit, true),
            (
                [1; 32],
                CloseConfirmation {
                    own_controls: Some(vec![]),
                    ..direct.clone()
                },
                false,
            ),
            (
                [1; 32],
                CloseConfirmation {
                    own_controls: Some(vec![control.clone(), control.clone()]),
                    ..direct.clone()
                },
                false,
            ),
            (
                [1; 32],
                CloseConfirmation {
                    own_controls: Some(vec![ConfirmedControl {
                        epoch: 2,
                        ..control.clone()
                    }]),
                    ..direct.clone()
                },
                false,
            ),
            (
                [1; 32],
                CloseConfirmation {
                    own_controls: Some(vec![ConfirmedControl {
                        session: None,
                        ..control.clone()
                    }]),
                    ..direct.clone()
                },
                false,
            ),
            (
                [1; 32],
                CloseConfirmation {
                    removes_workspace: false,
                    ..direct
                },
                false,
            ),
        ] {
            let mut state = original.clone();
            let outcome = state
                .apply(
                    device,
                    state.next_sequence(device),
                    Mutation::Close {
                        target,
                        confirmation: fence,
                    },
                    "/tmp".as_ref(),
                    |_| true,
                )
                .unwrap();
            assert_eq!(
                outcome,
                if accepted {
                    Outcome::Accepted { target: tab }
                } else {
                    Outcome::StaleConfirmation
                }
            );
            assert_eq!(state.inventory.workspaces.is_empty(), accepted);
        }
    }

    #[test]
    fn end_all_sessions_retains_exact_originals_across_acknowledgement_recovery_and_boots() {
        let mut state = State::empty();
        state.begin_boot([9; 32]);
        state
            .apply(
                [1; 32],
                1,
                Mutation::CreateWorkspace {
                    name: "Build".into(),
                    directory: None,
                    control: CreateControl::Creator,
                },
                "/home/test".as_ref(),
                |_| true,
            )
            .unwrap();
        let pane = state.inventory.workspaces[0].tabs[0].root_pane;
        let original = CloseInventory {
            controls: Vec::new(),
            boot: [9; 32],
            sessions: vec![ConfirmedPane {
                id: pane,
                session: Some([7; 32]),
            }],
        };
        state.inventory.close = Some(original.clone());
        assert_eq!(
            state
                .apply(
                    [2; 32],
                    1,
                    Mutation::EndAllSessions {
                        confirmation: CloseInventory {
                            boot: [8; 32],
                            ..original.clone()
                        }
                    },
                    "/home/test".as_ref(),
                    |_| true
                )
                .unwrap(),
            Outcome::StaleConfirmation
        );
        assert!(state.session_ends().is_none());
        let mutation = Mutation::EndAllSessions {
            confirmation: original.clone(),
        };
        state
            .apply([2; 32], 2, mutation.clone(), "/home/test".as_ref(), |_| {
                true
            })
            .unwrap();
        let outcome = state.outcome([2; 32], 2).unwrap().clone();
        state.acknowledge([2; 32], 1).unwrap();
        state.acknowledge([2; 32], 2).unwrap();
        let envelope = state.envelope("/home/test".as_ref()).unwrap();
        let record = serde_json::from_slice(&serde_json::to_vec(&envelope).unwrap()).unwrap();
        let mut recovered = State::from_organization(&record);
        recovered.validate().unwrap();
        recovered.begin_boot([9; 32]);
        assert_eq!(
            recovered.session_ends(),
            Some(&original),
            "acknowledgement cannot erase cleanup intent"
        );
        assert_eq!(outcome.outcome, Outcome::Accepted { target: [9; 32] });
        recovered.begin_boot([10; 32]);
        assert!(
            recovered.session_ends().is_none(),
            "old cleanup has no authority in another boot"
        );
        assert_eq!(
            recovered
                .apply([2; 32], 3, mutation, "/home/test".as_ref(), |_| true)
                .unwrap(),
            Outcome::StaleConfirmation
        );
    }

    #[test]
    fn confirmed_worktree_close_is_exact_and_keeps_outside_tabs_and_workspaces() {
        let mut state = State::empty();
        state
            .apply(
                [1; 32],
                1,
                Mutation::CreateWorkspace {
                    name: "Build".into(),
                    directory: None,
                    control: CreateControl::Creator,
                },
                "/home/test".as_ref(),
                |_| true,
            )
            .unwrap();
        let workspace = state.inventory.workspaces[0].id;
        let outside = state.inventory.workspaces[0].root_tab;
        let outside_pane = state.inventory.workspaces[0].tabs[0].root_pane;
        let mut anchor = [0; 32];
        for _ in 0..2 {
            anchor = state
                .add_worktree_tab(
                    Some([1; 32]),
                    workspace,
                    "/repo/main".into(),
                    "/repo/feature".into(),
                    "/repo/feature".into(),
                )
                .unwrap();
        }
        let Outcome::Accepted { target: other } = state
            .apply(
                [1; 32],
                2,
                Mutation::CreateWorkspace {
                    name: "Other".into(),
                    directory: None,
                    control: CreateControl::Creator,
                },
                "/home/test".as_ref(),
                |_| true,
            )
            .unwrap()
        else {
            panic!("other Workspace");
        };
        state
            .add_worktree_tab(
                Some([1; 32]),
                other,
                "/repo/main".into(),
                "/repo/feature".into(),
                "/repo/feature".into(),
            )
            .unwrap();
        state.inventory.close = Some(CloseInventory {
            controls: Vec::new(),
            boot: [9; 32],
            sessions: state
                .inventory
                .workspaces
                .iter()
                .flat_map(|w| &w.tabs)
                .flat_map(|t| &t.panes)
                .map(|p| ConfirmedPane {
                    id: p.id,
                    session: Some(p.id),
                })
                .collect(),
        });
        let target = CloseTarget::Worktree {
            workspace,
            tab: anchor,
        };
        let confirmation = state.inventory.close_confirmation(target).unwrap();
        assert_eq!(confirmation.panes.len(), 2);
        let changed = confirmation.panes[0].id;
        state
            .inventory
            .close
            .as_mut()
            .unwrap()
            .sessions
            .iter_mut()
            .find(|p| p.id == changed)
            .unwrap()
            .session = Some([8; 32]);
        assert_eq!(
            state
                .apply(
                    [1; 32],
                    3,
                    Mutation::Close {
                        target,
                        confirmation
                    },
                    "/home/test".as_ref(),
                    |_| true
                )
                .unwrap(),
            Outcome::StaleConfirmation
        );
        assert_eq!(state.inventory.workspaces[0].tabs.len(), 3);
        let confirmation = state.inventory.close_confirmation(target).unwrap();
        state
            .apply(
                [1; 32],
                4,
                Mutation::Close {
                    target,
                    confirmation,
                },
                "/home/test".as_ref(),
                |_| true,
            )
            .unwrap();
        assert_eq!(
            state.inventory.workspaces[0]
                .tabs
                .iter()
                .map(|t| t.id)
                .collect::<Vec<_>>(),
            [outside]
        );
        assert_eq!(
            state.inventory.workspaces[1].tabs.len(),
            2,
            "another Workspace's matching Worktree stays untouched"
        );
        let tab = state
            .add_worktree_tab(
                Some([1; 32]),
                workspace,
                "/repo/main".into(),
                "/repo/feature".into(),
                "/repo/feature".into(),
            )
            .unwrap();
        state.remove_pane(outside_pane);
        state.inventory.close = Some(CloseInventory {
            controls: Vec::new(),
            boot: [9; 32],
            sessions: state
                .inventory
                .workspaces
                .iter()
                .flat_map(|w| &w.tabs)
                .flat_map(|t| &t.panes)
                .map(|p| ConfirmedPane {
                    id: p.id,
                    session: None,
                })
                .collect(),
        });
        let target = CloseTarget::Worktree { workspace, tab };
        let confirmation = state.inventory.close_confirmation(target).unwrap();
        assert_eq!(
            state
                .apply(
                    [1; 32],
                    5,
                    Mutation::Close {
                        target,
                        confirmation
                    },
                    "/home/test".as_ref(),
                    |_| true
                )
                .unwrap(),
            Outcome::Invalid
        );
        assert_eq!(
            state.inventory.workspaces[0].tabs.len(),
            1,
            "no replacement Tab or shell"
        );
    }

    #[test]
    fn automatic_names_follow_the_root_directory_and_pin_but_custom_names_do_not() {
        let mut state = State::empty();
        assert!(matches!(
            state
                .apply(
                    [1; 32],
                    1,
                    Mutation::CreateWorkspace {
                        name: String::new(),
                        directory: None,
                        control: CreateControl::Creator,
                    },
                    "/home/test".as_ref(),
                    |_| true
                )
                .unwrap(),
            Outcome::Accepted { .. }
        ));
        let workspace = state.inventory.workspaces[0].id;
        let tab = state.inventory.workspaces[0].root_tab;
        let root = state.inventory.workspaces[0].tabs[0].root_pane;
        assert_eq!(state.inventory.workspaces[0].name, "Default");
        let Outcome::Accepted { target: split } = state
            .apply(
                [1; 32],
                2,
                Mutation::SplitPane {
                    workspace,
                    tab,
                    source: root,
                    axis: Axis::Right,
                    control: CreateControl::Creator,
                },
                "/home/test".as_ref(),
                |_| true,
            )
            .unwrap()
        else {
            panic!("split");
        };
        state.confirm_directory(split, "/other/ignored".into());
        assert_eq!(state.inventory.workspaces[0].name, "Default");
        state.confirm_directory(root, "/projects/api".into());
        assert_eq!(state.inventory.workspaces[0].name, "api");
        for (version, pin, expected) in [
            (0, Some("/projects/pinned".into()), "pinned"),
            (1, None, "api"),
        ] {
            state
                .apply(
                    [1; 32],
                    3 + version,
                    Mutation::PinWorkspace {
                        workspace,
                        version,
                        directory: pin,
                    },
                    "/home/test".as_ref(),
                    |_| true,
                )
                .unwrap();
            assert_eq!(state.inventory.workspaces[0].name, expected);
        }
        state = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        state.remove_pane(root);
        assert_eq!(state.inventory.workspaces[0].name, "ignored");
        state
            .apply(
                [1; 32],
                5,
                Mutation::RenameWorkspace {
                    workspace,
                    version: state.inventory.workspaces[0].name_version,
                    name: "Build".into(),
                },
                "/home/test".as_ref(),
                |_| true,
            )
            .unwrap();
        state.confirm_directory(split, "/home/test".into());
        assert_eq!(state.inventory.workspaces[0].name, "Build");
    }

    #[test]
    fn automatic_name_collisions_keep_survivors_and_reserve_custom_names() {
        let mut state = State::empty();
        for sequence in 1..=3 {
            state
                .apply(
                    [1; 32],
                    sequence,
                    Mutation::CreateWorkspace {
                        name: String::new(),
                        directory: None,
                        control: CreateControl::Creator,
                    },
                    "/home/test".as_ref(),
                    |_| true,
                )
                .unwrap();
        }
        let pane = state.inventory.workspaces[0].tabs[0].root_pane;
        state.remove_pane(pane);
        state
            .apply(
                [1; 32],
                4,
                Mutation::CreateWorkspace {
                    name: String::new(),
                    directory: None,
                    control: CreateControl::Creator,
                },
                "/home/test".as_ref(),
                |_| true,
            )
            .unwrap();
        assert_eq!(
            state
                .inventory
                .workspaces
                .iter()
                .map(|w| w.name.as_str())
                .collect::<Vec<_>>(),
            ["Default (2)", "Default (3)", "Default"]
        );
        let workspace = state.inventory.workspaces[2].id;
        state
            .apply(
                [1; 32],
                5,
                Mutation::RenameWorkspace {
                    workspace,
                    version: state.inventory.workspaces[2].name_version,
                    name: "Default (2)".into(),
                },
                "/home/test".as_ref(),
                |_| true,
            )
            .unwrap();
        assert_eq!(
            state
                .inventory
                .workspaces
                .iter()
                .map(|w| w.name.as_str())
                .collect::<Vec<_>>(),
            ["Default", "Default (3)", "Default (2)"]
        );
    }

    #[test]
    fn new_tabs_refuse_an_unavailable_pin_and_keep_the_refusal_after_repair() {
        let mut state = State::empty();
        let pin = Path::new("/repo/pin");
        let home = Path::new("/home/test");
        let Outcome::Accepted { target: workspace } = state
            .apply(
                [1; 32],
                1,
                Mutation::CreateWorkspace {
                    name: "Build".into(),
                    directory: Some(pin.into()),
                    control: CreateControl::Creator,
                },
                home,
                |_| true,
            )
            .unwrap()
        else {
            panic!("create the pinned Workspace");
        };
        let inventory = state.inventory.clone();
        let create = Mutation::CreateTab {
            workspace,
            source: None,
            control: CreateControl::Creator,
        };
        assert_eq!(
            state
                .apply([1; 32], 2, create.clone(), home, |path| {
                    assert_eq!(path, pin);
                    false
                })
                .unwrap(),
            Outcome::Invalid
        );
        assert_eq!(state.inventory, inventory);
        let retained = serde_json::to_vec(&state).unwrap();
        let mut state: State = serde_json::from_slice(&retained).unwrap();
        assert_eq!(
            state
                .apply([1; 32], 2, create.clone(), home, |path| path == pin)
                .unwrap(),
            Outcome::Invalid,
            "repair does not retry a completed operation"
        );
        assert_eq!(serde_json::to_vec(&state).unwrap(), retained);
        let accepted = state
            .apply([1; 32], 3, create.clone(), home, |path| path == pin)
            .unwrap();
        assert!(matches!(accepted, Outcome::Accepted { .. }));
        assert_eq!(
            state
                .apply([1; 32], 3, create, home, |path| path == pin)
                .unwrap(),
            accepted
        );
        assert_eq!(state.inventory.workspaces[0].tabs.len(), 2);
        assert_eq!(
            state.inventory.workspaces[0].tabs[1].panes[0].starting_directory,
            pin
        );
    }

    #[test]
    fn new_tabs_follow_the_source_pane_unless_the_workspace_is_pinned() {
        let mut state = State::empty();
        state
            .apply(
                [1; 32],
                1,
                Mutation::CreateWorkspace {
                    name: "Build".into(),
                    directory: None,
                    control: CreateControl::Creator,
                },
                "/home/test".as_ref(),
                |_| true,
            )
            .unwrap();
        let workspace = state.inventory.workspaces[0].id;
        let tab = state.inventory.workspaces[0].root_tab;
        let root = state.inventory.workspaces[0].tabs[0].root_pane;
        let Outcome::Accepted { target: source } = state
            .apply(
                [1; 32],
                2,
                Mutation::SplitPane {
                    workspace,
                    tab,
                    source: root,
                    axis: Axis::Right,
                    control: CreateControl::Creator,
                },
                "/home/test".as_ref(),
                |_| true,
            )
            .unwrap()
        else {
            panic!("split");
        };
        state.confirm_directory(root, "/repo/main".into());
        state.confirm_directory(source, "/repo/feature".into());
        state
            .apply(
                [1; 32],
                3,
                Mutation::CreateTab {
                    workspace,
                    source: Some(source),
                    control: CreateControl::Creator,
                },
                "/home/test".as_ref(),
                |_| true,
            )
            .unwrap();
        assert_eq!(
            state.inventory.workspaces[0].tabs[1].panes[0].starting_directory,
            Path::new("/repo/feature")
        );
        state
            .apply(
                [1; 32],
                4,
                Mutation::PinWorkspace {
                    workspace,
                    version: 0,
                    directory: Some("/repo/pin".into()),
                },
                "/home/test".as_ref(),
                |_| true,
            )
            .unwrap();
        state
            .apply(
                [1; 32],
                5,
                Mutation::CreateTab {
                    workspace,
                    source: Some(source),
                    control: CreateControl::Creator,
                },
                "/home/test".as_ref(),
                |_| true,
            )
            .unwrap();
        assert_eq!(
            state.inventory.workspaces[0].tabs[2].panes[0].starting_directory,
            Path::new("/repo/pin")
        );
    }

    #[test]
    fn confirmed_close_promotes_roots_and_replay_cannot_remove_later_work() {
        let mut state = State::empty();
        state
            .apply(
                [1; 32],
                1,
                Mutation::CreateWorkspace {
                    name: "Build".into(),
                    directory: None,
                    control: CreateControl::Creator,
                },
                "/tmp".as_ref(),
                |_| true,
            )
            .unwrap();
        let workspace = state.inventory.workspaces[0].id;
        let tab = state.inventory.workspaces[0].root_tab;
        state
            .apply(
                [1; 32],
                2,
                Mutation::CreateTab {
                    workspace,
                    source: None,
                    control: CreateControl::Creator,
                },
                "/tmp".as_ref(),
                |_| true,
            )
            .unwrap();
        let survivor = state.inventory.workspaces[0].tabs[1].id;
        // As when the Tab opened in a Worktree; the promoted Root Tab follows its Root Pane.
        state.inventory.workspaces[0].tabs[1].worktree_membership = WorktreeMembership::Opened;
        let pane = state.inventory.workspaces[0].tabs[0].root_pane;
        state.inventory.close = Some(CloseInventory {
            controls: Vec::new(),
            boot: [9; 32],
            sessions: vec![ConfirmedPane {
                id: pane,
                session: None,
            }],
        });
        let close = Mutation::Close {
            target: CloseTarget::Tab { workspace, tab },
            confirmation: CloseConfirmation {
                own_controls: None,
                boot: [9; 32],
                removes_tab: true,
                removes_workspace: false,
                panes: vec![ConfirmedPane {
                    id: pane,
                    session: None,
                }],
            },
        };
        assert_eq!(
            state
                .apply([2; 32], 1, close.clone(), "/tmp".as_ref(), |_| true)
                .unwrap(),
            Outcome::Accepted { target: tab }
        );
        assert_eq!(state.inventory.workspaces[0].root_tab, survivor);
        assert_eq!(state.inventory.workspaces[0].tabs.len(), 1);
        assert_eq!(
            state.inventory.workspaces[0].tabs[0].worktree_membership,
            WorktreeMembership::Following
        );
        assert_eq!(
            state
                .apply([2; 32], 1, close, "/tmp".as_ref(), |_| true)
                .unwrap(),
            Outcome::Accepted { target: tab }
        );
        assert_eq!(state.inventory.workspaces[0].tabs.len(), 1);
    }

    #[test]
    fn conflicting_rename_preserves_draft_and_independent_creation_does_not_conflict() {
        let mut state = State::empty();
        let create = Mutation::CreateWorkspace {
            name: "Build".into(),
            directory: None,
            control: CreateControl::Creator,
        };
        let Outcome::Accepted { target: workspace } = state
            .apply([1; 32], 1, create, "/tmp".as_ref(), |_| true)
            .unwrap()
        else {
            panic!()
        };
        let rename = Mutation::RenameWorkspace {
            workspace,
            version: 0,
            name: "Logs".into(),
        };
        state
            .apply(
                [1; 32],
                2,
                Mutation::CreateTab {
                    workspace,
                    source: None,
                    control: CreateControl::Creator,
                },
                "/tmp".as_ref(),
                |_| true,
            )
            .unwrap();
        assert!(matches!(
            state
                .apply([1; 32], 3, rename.clone(), "/tmp".as_ref(), |_| true)
                .unwrap(),
            Outcome::Accepted { .. }
        ));
        let rejected = Mutation::RenameWorkspace {
            workspace,
            version: 0,
            name: "Draft".into(),
        };
        assert_eq!(
            state
                .apply([2; 32], 1, rejected.clone(), "/tmp".as_ref(), |_| true)
                .unwrap(),
            Outcome::Conflict
        );
        assert_eq!(state.outcome([2; 32], 1).unwrap().mutation, rejected);
        assert_eq!(state.inventory.workspaces[0].name, "Logs");
    }

    #[test]
    fn replay_and_acknowledged_identity_cannot_duplicate_creation() {
        let mut state = State::empty();
        let create = Mutation::CreateWorkspace {
            name: "Build".into(),
            directory: None,
            control: CreateControl::Creator,
        };
        let first = state
            .apply([1; 32], 1, create.clone(), "/tmp".as_ref(), |_| true)
            .unwrap();
        assert_eq!(
            state
                .apply([1; 32], 1, create.clone(), "/tmp".as_ref(), |_| true)
                .unwrap(),
            first
        );
        state.acknowledge([1; 32], 1).unwrap();
        assert!(
            state
                .apply([1; 32], 1, create, "/tmp".as_ref(), |_| true)
                .is_err()
        );
        assert_eq!(state.inventory.workspaces.len(), 1);
    }

    #[test]
    fn a_repeated_acknowledgement_retires_nothing_else() {
        let mut state = State::empty();
        assert_eq!(
            state.acknowledge([1; 32], 4),
            Ok(()),
            "nothing recorded yet"
        );
        let create = |name: &str| Mutation::CreateWorkspace {
            name: name.into(),
            directory: None,
            control: CreateControl::Creator,
        };
        state
            .apply([1; 32], 1, create("First"), "/tmp".as_ref(), |_| true)
            .unwrap();
        state.acknowledge([1; 32], 1).unwrap();
        state
            .apply([1; 32], 2, create("Second"), "/tmp".as_ref(), |_| true)
            .unwrap();
        let before = state.clone();
        assert_eq!(state.acknowledge([1; 32], 1), Ok(()));
        assert!(state == before, "the newer outcome stays recorded");
        assert!(state.outcome([1; 32], 2).is_some());
    }

    #[test]
    fn a_lost_acknowledgement_never_stops_later_edits() {
        let mut state = State::empty();
        let create = |sequence: u64| Mutation::CreateWorkspace {
            name: format!("Build {sequence}"),
            directory: None,
            control: CreateControl::Creator,
        };
        state
            .apply([1; 32], 1, create(1), "/tmp".as_ref(), |_| true)
            .unwrap();
        // The acknowledgement of edit 1 never arrives. Another client keeps one outcome open.
        state
            .apply([2; 32], 1, create(1), "/tmp".as_ref(), |_| true)
            .unwrap();
        for sequence in 2..=(MAX_OUTCOMES as u64 + 8) {
            state
                .apply([1; 32], sequence, create(sequence), "/tmp".as_ref(), |_| {
                    true
                })
                .unwrap();
            state.acknowledge([1; 32], sequence).unwrap();
            assert!(
                state.outcome([1; 32], 1).is_none(),
                "a later acknowledgement retires it"
            );
        }
        assert!(
            state.outcome([2; 32], 1).is_some(),
            "only the acknowledging client's"
        );
        assert_eq!(state.next_sequence([1; 32]), MAX_OUTCOMES as u64 + 9);
    }

    #[test]
    fn pending_outcomes_are_bounded_without_forgetting_unacknowledged_creations() {
        let mut state = State::empty();
        for sequence in 1..=32 {
            state
                .apply(
                    [1; 32],
                    sequence,
                    Mutation::CreateWorkspace {
                        name: "Build".into(),
                        directory: None,
                        control: CreateControl::Creator,
                    },
                    "/tmp".as_ref(),
                    |_| true,
                )
                .unwrap();
        }
        let create = Mutation::CreateWorkspace {
            name: "More".into(),
            directory: None,
            control: CreateControl::Creator,
        };
        assert!(matches!(
            state.apply([1; 32], 33, create.clone(), "/tmp".as_ref(), |_| true),
            Err(Refusal::Capacity)
        ));
        assert!(state.outcome([1; 32], 1).is_some());
        state.acknowledge([1; 32], 1).unwrap();
        state
            .apply([1; 32], 33, create, "/tmp".as_ref(), |_| true)
            .unwrap();
        assert_eq!(state.inventory.workspaces.len(), 33);
    }

    #[test]
    fn a_full_device_table_replaces_the_least_recently_editing_device_with_nothing_to_acknowledge()
    {
        let mut state = State::empty();
        // Any recorded outcome counts. A missing Workspace keeps the inventory empty.
        let create = Mutation::RenameWorkspace {
            workspace: [9; 32],
            version: 0,
            name: "Build".into(),
        };
        let device = |n: usize| [u8::try_from(n).unwrap(); 32];
        // Device 0 never acknowledges its edit. Every other device does.
        for n in 0..MAX_DEVICES {
            state
                .apply(device(n), 1, create.clone(), "/tmp".as_ref(), |_| true)
                .unwrap();
            if n != 0 {
                state.acknowledge(device(n), 1).unwrap();
            }
        }
        // Device 1 edits again, so device 2 becomes the least recently editing idle device.
        state
            .apply(device(1), 2, create.clone(), "/tmp".as_ref(), |_| true)
            .unwrap();
        state.acknowledge(device(1), 2).unwrap();
        let newcomer = device(MAX_DEVICES);
        state
            .apply(newcomer, 1, create.clone(), "/tmp".as_ref(), |_| true)
            .unwrap();
        assert_eq!(state.devices.len(), MAX_DEVICES);
        assert!(state.outcome(newcomer, 1).is_some());
        assert!(
            state.outcome(device(0), 1).is_some(),
            "an unacknowledged outcome keeps its device"
        );
        assert_eq!(state.next_sequence(device(1)), 3);
        assert!(state.devices.iter().all(|d| d.device != device(2)));
        // The replaced device's repeated acknowledgement retires nothing.
        assert_eq!(state.acknowledge(device(2), 1), Ok(()));
        assert!(state.validate().is_ok());
    }

    #[test]
    fn a_replaced_device_still_submits_its_unresolved_operation_under_its_original_sequence() {
        let mut state = State::empty();
        let create = Mutation::RenameWorkspace {
            workspace: [9; 32],
            version: 0,
            name: "Build".into(),
        };
        let device = |n: usize| [u8::try_from(n).unwrap(); 32];
        // Device 0 settles edit 1. Its edit 2 is journaled but never reaches the server.
        state
            .apply(device(0), 1, create.clone(), "/tmp".as_ref(), |_| true)
            .unwrap();
        state.acknowledge(device(0), 1).unwrap();
        // Device 1 settles more edits, and both are replaced, which raises the floor to 4.
        for sequence in 1..=4 {
            state
                .apply(device(1), sequence, create.clone(), "/tmp".as_ref(), |_| {
                    true
                })
                .unwrap();
            state.acknowledge(device(1), sequence).unwrap();
        }
        for n in 2..=MAX_DEVICES + 1 {
            state
                .apply(device(n), 1, create.clone(), "/tmp".as_ref(), |_| true)
                .unwrap();
            state.acknowledge(device(n), 1).unwrap();
        }
        assert!(state.devices.iter().all(|d| d.device != device(0)));
        assert!(state.devices.iter().all(|d| d.device != device(1)));
        assert_eq!(state.next_sequence(device(0)), 5);
        // The retry of edit 2 applies once, and its outcome follows ADR 0037.
        let rename = Mutation::RenameWorkspace {
            workspace: [9; 32],
            version: 0,
            name: "Retried".into(),
        };
        assert_eq!(
            state.apply(device(0), 2, rename.clone(), "/tmp".as_ref(), |_| true),
            Ok(Outcome::Missing)
        );
        assert_eq!(
            state.apply(device(0), 2, rename, "/tmp".as_ref(), |_| true),
            Ok(Outcome::Missing),
            "a repeated retry replays the recorded outcome"
        );
        assert_eq!(state.next_sequence(device(0)), 3);
        assert!(matches!(
            state.apply(device(0), 5, create.clone(), "/tmp".as_ref(), |_| true),
            Err(Refusal::Stale)
        ));
        // The repeated acknowledgement of edit 1 retires nothing.
        state.acknowledge(device(0), 1).unwrap();
        assert!(state.outcome(device(0), 2).is_some());
        assert!(matches!(
            state.apply(device(0), 0, create, "/tmp".as_ref(), |_| true),
            Err(Refusal::Stale)
        ));
        assert!(state.validate().is_ok());
    }

    #[test]
    fn forgetting_a_device_frees_its_outcomes_once_its_worktree_write_settles() {
        let mut state = State::empty();
        for sequence in 1..MAX_OUTCOMES as u64 {
            state
                .apply(
                    [1; 32],
                    sequence,
                    Mutation::CreateWorkspace {
                        name: "Build".into(),
                        directory: None,
                        control: CreateControl::Creator,
                    },
                    "/tmp".as_ref(),
                    |_| true,
                )
                .unwrap();
        }
        let location = Mutation::Worktree(crate::worktree::Operation::SetLocation {
            version: 0,
            template: "{repository}-{branch}".into(),
        });
        let sequence = MAX_OUTCOMES as u64;
        assert_eq!(
            state.apply([1; 32], sequence, location, "/tmp".as_ref(), |_| true),
            Ok(Outcome::Pending)
        );
        let create = Mutation::CreateWorkspace {
            name: "Other".into(),
            directory: None,
            control: CreateControl::Creator,
        };
        assert!(matches!(
            state.apply([2; 32], 1, create.clone(), "/tmp".as_ref(), |_| true),
            Err(Refusal::Capacity)
        ));
        let before = state.clone();
        assert!(!state.forget([1; 32]), "a running write keeps its record");
        assert!(state == before);
        state
            .finish_worktree([1; 32], sequence, Outcome::Accepted { target: [1; 32] })
            .unwrap();
        assert!(state.forget([1; 32]));
        assert!(state.outcome([1; 32], 1).is_none());
        assert_eq!(state.next_sequence([1; 32]), 1);
        state
            .apply([2; 32], 1, create, "/tmp".as_ref(), |_| true)
            .unwrap();
        assert!(state.validate().is_ok());
    }

    #[test]
    fn creating_a_workspace_at_a_directory_pins_it_and_roots_its_first_pane_there() {
        let mut state = State::empty();
        let create = |directory: &str| Mutation::CreateWorkspace {
            name: "Site".into(),
            directory: Some(directory.into()),
            control: CreateControl::Creator,
        };
        assert_eq!(
            state
                .apply([1; 32], 1, create("/missing"), "/home".as_ref(), |_| false)
                .unwrap(),
            Outcome::Invalid
        );
        assert!(state.inventory.workspaces.is_empty());
        assert!(!create("relative").is_bounded());
        state
            .apply([1; 32], 2, create("/srv/site"), "/home".as_ref(), |path| {
                path == Path::new("/srv/site")
            })
            .unwrap();
        let workspace = &state.inventory.workspaces[0];
        assert_eq!(workspace.pinned_directory, Some("/srv/site".into()));
        assert_eq!(
            workspace.tabs[0].panes[0].starting_directory,
            Path::new("/srv/site")
        );
    }

    #[test]
    fn stale_pin_preserves_the_conflict_even_if_its_draft_directory_became_unavailable() {
        let mut state = State::empty();
        let Outcome::Accepted { target: workspace } = state
            .apply(
                [1; 32],
                1,
                Mutation::CreateWorkspace {
                    name: "Build".into(),
                    directory: None,
                    control: CreateControl::Creator,
                },
                "/tmp".as_ref(),
                |_| true,
            )
            .unwrap()
        else {
            panic!()
        };
        state
            .apply(
                [1; 32],
                2,
                Mutation::PinWorkspace {
                    workspace,
                    version: 0,
                    directory: Some("/".into()),
                },
                "/tmp".as_ref(),
                |_| true,
            )
            .unwrap();
        assert_eq!(
            state
                .apply(
                    [2; 32],
                    1,
                    Mutation::PinWorkspace {
                        workspace,
                        version: 0,
                        directory: Some("/missing".into())
                    },
                    "/tmp".as_ref(),
                    |_| false
                )
                .unwrap(),
            Outcome::Conflict
        );
    }

    #[test]
    fn recovery_preserves_creator_reservations_only_in_the_boot_that_created_them() {
        let mut state = State::empty();
        state.begin_boot([7; 32]);
        state
            .apply(
                [1; 32],
                1,
                Mutation::CreateWorkspace {
                    name: "Build".into(),
                    directory: None,
                    control: CreateControl::Creator,
                },
                "/tmp".as_ref(),
                |_| true,
            )
            .unwrap();
        state.inventory.workspaces[0].tabs[0].panes[0].observers = PaneObservers {
            names: vec!["Observer CLI".into()],
            others: 4,
        };
        let envelope = state.envelope("/tmp".as_ref()).unwrap();
        assert_eq!(
            envelope.shared.unwrap().inventory.workspaces[0].tabs[0].panes[0].observers,
            PaneObservers::default()
        );
        let bytes = serde_json::to_vec(&state).unwrap();
        let mut same_boot: State = serde_json::from_slice(&bytes).unwrap();
        same_boot.begin_boot([7; 32]);
        assert_eq!(
            same_boot.inventory.workspaces[0].tabs[0].panes[0].creator,
            Some([1; 32])
        );
        let mut next_boot: State = serde_json::from_slice(&bytes).unwrap();
        next_boot.begin_boot([8; 32]);
        assert_eq!(
            next_boot.inventory.workspaces[0].tabs[0].panes[0].observers,
            PaneObservers::default()
        );
        assert_eq!(
            next_boot.inventory.workspaces[0].tabs[0].panes[0].creator,
            None
        );
    }

    #[test]
    fn deleting_a_split_source_retains_only_surviving_defaults_and_promotes_roots() {
        let mut state = State::empty();
        let Outcome::Accepted { target: workspace } = state
            .apply(
                [1; 32],
                1,
                Mutation::CreateWorkspace {
                    name: "Build".into(),
                    directory: None,
                    control: CreateControl::Creator,
                },
                "/tmp".as_ref(),
                |_| true,
            )
            .unwrap()
        else {
            panic!()
        };
        let tab = state.inventory.workspaces[0].root_tab;
        let source = state.inventory.workspaces[0].tabs[0].root_pane;
        let Outcome::Accepted { target: pane } = state
            .apply(
                [1; 32],
                2,
                Mutation::SplitPane {
                    workspace,
                    tab,
                    source,
                    axis: Axis::Down,
                    control: CreateControl::Creator,
                },
                "/tmp".as_ref(),
                |_| true,
            )
            .unwrap()
        else {
            panic!()
        };
        state.remove_pane(source);
        let envelope = state.envelope("/tmp".as_ref()).unwrap();
        let reopened: crate::organization::Organization =
            serde_json::from_slice(&serde_json::to_vec(&envelope).unwrap()).unwrap();
        let mut restored = State::from_organization(&reopened);
        restored.validate().unwrap();
        let tab = &restored.inventory.workspaces[0].tabs[0];
        assert_eq!(tab.root_pane, pane);
        assert_eq!(tab.panes.len(), 1);
        assert_eq!(tab.panes[0].source, None);
        assert_eq!(tab.panes[0].axis, Axis::Down);
        restored.remove_pane(pane);
        assert_eq!(
            restored
                .apply(
                    [1; 32],
                    3,
                    Mutation::CreateTab {
                        workspace,
                        source: None,
                        control: CreateControl::Creator,
                    },
                    "/tmp".as_ref(),
                    |_| true
                )
                .unwrap(),
            Outcome::Missing
        );
        assert!(restored.inventory.workspaces.is_empty());
    }

    #[cfg(feature = "client")]
    mod journal {
        use super::*;

        const SERVER: Id = [1; 32];
        const CLIENT: Id = [2; 32];

        /// A private directory for one client's journal, removed when the test ends.
        struct Root(PathBuf);
        impl Root {
            fn new() -> Self {
                Self(std::env::temp_dir().canonicalize().unwrap().join(format!(
                    "st-journal-{:x}",
                    u64::from_be_bytes(crate::protocol::random().unwrap()[..8].try_into().unwrap())
                )))
            }
            fn open(&self) -> ClientJournal {
                let directory = crate::storage::Directory::open(&self.0, true).unwrap();
                ClientJournal::open(directory, SERVER, CLIENT).unwrap()
            }
        }
        impl Drop for Root {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }

        fn rename(name: &str) -> Mutation {
            Mutation::RenameWorkspace {
                workspace: [3; 32],
                version: 0,
                name: name.into(),
            }
        }

        fn settle(journal: &mut ClientJournal, sequence: u64, outcome: Outcome) -> RetainedOutcome {
            let mutation = rename(&format!("Draft {sequence}"));
            journal.submit(sequence, mutation.clone()).unwrap();
            let retained = RetainedOutcome {
                sequence,
                mutation,
                outcome,
            };
            journal.retire(retained.clone()).unwrap();
            retained
        }

        /// A refused rename waits behind a dialog while a later operation retires. Both survive a
        /// relaunch until the client presents each, and a presented one never returns.
        #[test]
        fn a_later_operation_should_keep_an_earlier_unseen_outcome_across_a_relaunch() {
            let root = Root::new();
            let mut journal = root.open();
            let conflict = settle(&mut journal, 1, Outcome::Conflict);
            settle(&mut journal, 2, Outcome::Accepted { target: [4; 32] });
            let failed = settle(
                &mut journal,
                3,
                Outcome::WorktreeFailed(crate::worktree::Error::Location),
            );
            drop(journal);

            let mut relaunched = root.open();
            let recovered = relaunched.unseen().to_vec();
            relaunched.presented(failed.sequence).unwrap();
            drop(relaunched);
            let later = root.open();

            assert_eq!(recovered, vec![conflict.clone(), failed]);
            assert_eq!(later.unseen(), [conflict]);
            assert_eq!(
                later.last().map(|last| last.sequence),
                Some(3),
                "the transport acknowledgement still follows the last retired operation"
            );
        }

        #[test]
        fn unseen_outcomes_should_be_bounded_by_counting_the_ones_forgotten() {
            let root = Root::new();
            let mut journal = root.open();
            let total = UNSEEN_OUTCOMES as u64 + 2;
            for sequence in 1..=total {
                settle(&mut journal, sequence, Outcome::Invalid);
            }

            drop(journal);
            let mut relaunched = root.open();
            let kept: Vec<u64> = relaunched
                .unseen()
                .iter()
                .map(|unseen| unseen.sequence)
                .collect();
            let forgotten = relaunched.forgotten();
            relaunched.forgotten_presented(2).unwrap();
            settle(&mut relaunched, total + 1, Outcome::Invalid);
            drop(relaunched);

            assert_eq!(kept, (3..=total).collect::<Vec<_>>());
            assert_eq!(forgotten, (2, 0), "the relaunch can say two went unseen");
            assert_eq!(
                root.open().forgotten(),
                (3, 2),
                "only the one forgotten after that notice still needs one"
            );
        }

        /// A journal written before the client recorded unseen outcomes still opens, with none.
        #[test]
        fn a_journal_without_unseen_outcomes_should_open_with_none() {
            let root = Root::new();
            drop(root.open());
            let directory = crate::storage::Directory::open(&root.0, false).unwrap();
            directory
                .write_json(
                    c"operations.json",
                    &serde_json::json!({
                        "server": SERVER,
                        "client": CLIENT,
                        "pending": null,
                        "last": null,
                    }),
                )
                .unwrap();

            assert!(root.open().unseen().is_empty());
        }
    }

    #[test]
    fn created_work_is_controlled_as_its_create_control_says() {
        let mut state = State::empty();
        let Outcome::Accepted { target: workspace } = state
            .apply(
                [1; 32],
                1,
                Mutation::CreateWorkspace {
                    name: "Build".into(),
                    directory: None,
                    control: CreateControl::Unclaimed,
                },
                "/tmp".as_ref(),
                |_| true,
            )
            .unwrap()
        else {
            panic!("the Workspace is created");
        };
        let origin = state.inventory.workspaces[0].tabs[0].root_pane;
        assert_eq!(state.pane(origin).unwrap().2.creator, None);
        // The publication at commit carries the origin's live control grant.
        state.inventory.close = Some(CloseInventory {
            boot: [0; 32],
            sessions: Vec::new(),
            controls: vec![ConfirmedControl {
                pane: origin,
                session: Some([5; 32]),
                owner: [7; 32],
                epoch: 1,
            }],
        });
        let creators = [
            (CreateControl::Creator, Some([2; 32])),
            (
                CreateControl::OriginController { pane: origin },
                Some([7; 32]),
            ),
            (CreateControl::OriginController { pane: [9; 32] }, None),
        ];
        for (sequence, (control, creator)) in (1..).zip(creators) {
            let Outcome::Accepted { target: tab } = state
                .apply(
                    [2; 32],
                    sequence,
                    Mutation::CreateTab {
                        workspace,
                        source: None,
                        control,
                    },
                    "/tmp".as_ref(),
                    |_| true,
                )
                .unwrap()
            else {
                panic!("the Tab is created");
            };
            let placement = state.inventory.placement(tab).unwrap();
            assert_eq!((placement.workspace, placement.tab), (workspace, tab));
            assert_eq!(state.pane(placement.pane).unwrap().2.creator, creator);
        }
    }
}
