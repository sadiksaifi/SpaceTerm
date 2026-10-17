//! Authoritative shared membership and bounded, durable mutation outcomes.

use crate::{ServerError, pane::Refusal};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub type Id = [u8; 32];
pub const MAX_WORKSPACES: usize = 64;
pub const MAX_TABS: usize = 128;
pub const MAX_PANES: usize = 128;
const MAX_OUTCOMES: usize = 32;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Inventory {
    /// Live owning-server Git facts. Excluded from retained Organization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repositories: Option<crate::repository::Snapshot>,
    pub revision: u64,
    pub workspaces: Vec<Workspace>,
    /// Live facts accompany publication only; retained organization never supplies authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub close: Option<CloseInventory>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CloseTarget {
    Pane { workspace: Id, tab: Id, pane: Id },
    Tab { workspace: Id, tab: Id },
    Workspace { workspace: Id },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfirmedPane {
    pub id: Id,
    pub session: Option<Id>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloseInventory {
    pub boot: Id,
    pub sessions: Vec<ConfirmedPane>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloseConfirmation {
    pub boot: Id,
    pub panes: Vec<ConfirmedPane>,
    pub removes_tab: bool,
    pub removes_workspace: bool,
}

impl Inventory {
    pub fn close_confirmation(&self, target: CloseTarget) -> Option<CloseConfirmation> {
        let live = self.close.as_ref()?;
        let ids = self.close_panes(target)?;
        let workspace = match target {
            CloseTarget::Pane { workspace, .. }
            | CloseTarget::Tab { workspace, .. }
            | CloseTarget::Workspace { workspace } => workspace,
        };
        let workspace = self.workspaces.iter().find(|w| w.id == workspace)?;
        // Sibling removal can change the consequences without changing any target Pane.
        let removes_tab = match target {
            CloseTarget::Pane { tab, .. } => {
                workspace.tabs.iter().find(|t| t.id == tab)?.panes.len() == 1
            }
            CloseTarget::Tab { .. } | CloseTarget::Workspace { .. } => true,
        };
        let removes_workspace =
            ids.len() == workspace.tabs.iter().map(|t| t.panes.len()).sum::<usize>();
        let panes = ids
            .into_iter()
            .map(|id| live.sessions.iter().find(|p| p.id == id).cloned())
            .collect::<Option<Vec<_>>>()?;
        Some(CloseConfirmation {
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
            | CloseTarget::Workspace { workspace } => workspace,
        };
        let w = self.workspaces.iter().find(|w| w.id == workspace)?;
        match target {
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tab {
    /// Directory repair must not move this Tab to a different Worktree, including its Root Pane.
    #[serde(default)]
    pub worktree_membership_fixed: bool,
    /// Saved membership, retained even when Git no longer lists the directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<PathBuf>,
    /// Main Worktree discovery locator for this membership, revalidated after each boot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_repository: Option<PathBuf>,
    pub id: Id,
    pub root_pane: Id,
    pub panes: Vec<Pane>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pane {
    pub id: Id,
    pub source: Option<Id>,
    pub axis: Axis,
    pub starting_directory: PathBuf,
    pub confirmed_directory: Option<PathBuf>,
    /// The creator reserves control even before measured first use starts a shell.
    pub creator: Option<Id>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Axis {
    Right,
    Down,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mutation {
    Close {
        target: CloseTarget,
        confirmation: CloseConfirmation,
    },
    CreateWorkspace {
        name: String,
    },
    CreateTab {
        workspace: Id,
    },
    SplitPane {
        workspace: Id,
        tab: Id,
        source: Id,
        axis: Axis,
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
    pub fn is_bounded(&self) -> bool {
        match self {
            Self::Close { confirmation, .. } => {
                !confirmation.panes.is_empty() && confirmation.panes.len() <= MAX_PANES
            }
            Self::CreateWorkspace { name } | Self::RenameWorkspace { name, .. } => {
                name.len() <= 256
            }
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
    Accepted { target: Id },
    Conflict,
    Missing,
    Invalid,
    Limit,
    StaleConfirmation,
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
pub struct State {
    pub inventory: Inventory,
    devices: Vec<DeviceOutcomes>,
    #[serde(default)]
    creator_boot: Option<Id>,
}
impl State {
    pub fn empty() -> Self {
        Self {
            inventory: Inventory {
                repositories: None,
                revision: 0,
                workspaces: Vec::new(),
                close: None,
            },
            devices: Vec::new(),
            creator_boot: None,
        }
    }
    pub fn begin_boot(&mut self, boot: Id) {
        if self.creator_boot != Some(boot) {
            for w in &mut self.inventory.workspaces {
                for t in &mut w.tabs {
                    for p in &mut t.panes {
                        p.creator = None;
                    }
                }
            }
            self.creator_boot = Some(boot);
        }
    }
    /// Legacy first-Pane opening has no shared-edit operation identity. It must not retire a
    /// sequence a concurrent main-window edit has already reserved in its durable journal.
    pub fn ensure_first_workspace(&mut self, device: Id, directory: &Path) -> Result<(), Refusal> {
        if self.inventory.workspaces.is_empty() {
            self.edit(
                device,
                &Mutation::CreateWorkspace {
                    name: "Development".into(),
                },
                directory,
                |_| true,
            )?;
        }
        Ok(())
    }
    pub fn from_legacy(org: &crate::organization::Organization) -> Self {
        org.shared.as_deref().cloned().unwrap_or_else(|| Self {
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
                        worktree_membership_fixed: false,
                        worktree_repository: None,
                        id: org.tab,
                        root_pane: org.pane,
                        panes: vec![Pane {
                            id: org.pane,
                            source: None,
                            axis: Axis::Right,
                            starting_directory: org.starting_directory.clone(),
                            confirmed_directory: org.confirmed_directory.clone(),
                            creator: None,
                        }],
                    }],
                }],
            },
            devices: Vec::new(),
            creator_boot: None,
        })
    }
    pub fn envelope(
        &self,
        directory: &Path,
    ) -> Result<crate::organization::Organization, ServerError> {
        // Legacy fields retain their original meaning for existing Development tooling.
        // Shared organization is the authority once present, including an empty inventory.
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
    pub fn next_sequence(&self, device: Id) -> u64 {
        self.devices
            .iter()
            .find(|d| d.device == device)
            .map_or(1, |d| {
                d.pending.last().map_or(d.retired + 1, |o| o.sequence + 1)
            })
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
        if sequence != self.next_sequence(device) || sequence == u64::MAX {
            return Err(Refusal::Stale);
        }
        if self.devices.iter().map(|d| d.pending.len()).sum::<usize>() >= MAX_OUTCOMES {
            return Err(Refusal::Capacity);
        }
        if !self.devices.iter().any(|d| d.device == device) {
            if self.devices.len() >= 64 {
                return Err(Refusal::Capacity);
            }
            self.devices.push(DeviceOutcomes {
                device,
                retired: 0,
                pending: Vec::new(),
            });
        }
        let outcome = self.edit(device, &mutation, directory, validate_pin)?;
        self.devices
            .iter_mut()
            .find(|d| d.device == device)
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
        let d = self
            .devices
            .iter_mut()
            .find(|d| d.device == device)
            .ok_or(Refusal::Missing)?;
        if sequence <= d.retired {
            return Ok(());
        }
        if d.pending.first().is_none_or(|o| o.sequence != sequence) {
            return Err(Refusal::Stale);
        }
        d.pending.remove(0);
        d.retired = sequence;
        Ok(())
    }
    fn edit(
        &mut self,
        device: Id,
        mutation: &Mutation,
        directory: &Path,
        validate_pin: impl FnOnce(&Path) -> bool,
    ) -> Result<Outcome, Refusal> {
        let new_id = || crate::protocol::random().map_err(|_| Refusal::Failed);
        let valid_name = |name: &str| {
            !name.trim().is_empty() && name.len() <= 256 && !name.chars().any(char::is_control)
        };
        let target = match mutation {
            Mutation::Close {
                target,
                confirmation,
            } => {
                let Some(ids) = self.inventory.close_panes(*target) else {
                    return Ok(Outcome::Missing);
                };
                if self.inventory.close_confirmation(*target).as_ref() != Some(confirmation) {
                    return Ok(Outcome::StaleConfirmation);
                }
                for id in ids {
                    self.remove_pane(id);
                }
                match target {
                    CloseTarget::Pane { pane, .. } => *pane,
                    CloseTarget::Tab { tab, .. } => *tab,
                    CloseTarget::Workspace { workspace } => *workspace,
                }
            }
            Mutation::CreateWorkspace { name } => {
                if !valid_name(name) {
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
                self.inventory.workspaces.push(Workspace {
                    id,
                    name: name.trim().to_owned(),
                    name_version: 0,
                    pin_version: 0,
                    pinned_directory: None,
                    root_tab: tab,
                    tabs: vec![Tab {
                        worktree: None,
                        worktree_membership_fixed: false,
                        worktree_repository: None,
                        id: tab,
                        root_pane: pane,
                        panes: vec![Pane {
                            id: pane,
                            source: None,
                            axis: Axis::Right,
                            starting_directory: directory.to_owned(),
                            confirmed_directory: None,
                            creator: Some(device),
                        }],
                    }],
                });
                id
            }
            Mutation::CreateTab { workspace } => {
                if self.tab_count() >= MAX_TABS || self.pane_count() >= MAX_PANES {
                    return Ok(Outcome::Limit);
                }
                let Some(w) = self
                    .inventory
                    .workspaces
                    .iter_mut()
                    .find(|w| w.id == *workspace)
                else {
                    return Ok(Outcome::Missing);
                };
                let start = w.pinned_directory.clone().unwrap_or_else(|| {
                    w.tabs
                        .iter()
                        .find(|t| t.id == w.root_tab)
                        .and_then(|t| t.panes.iter().find(|p| p.id == t.root_pane))
                        .map_or_else(
                            || directory.to_owned(),
                            |p| {
                                p.confirmed_directory
                                    .as_ref()
                                    .unwrap_or(&p.starting_directory)
                                    .clone()
                            },
                        )
                });
                let id = new_id()?;
                let pane = new_id()?;
                w.tabs.push(Tab {
                    worktree: None,
                    worktree_membership_fixed: false,
                    worktree_repository: None,
                    id,
                    root_pane: pane,
                    panes: vec![Pane {
                        id: pane,
                        source: None,
                        axis: Axis::Right,
                        starting_directory: start,
                        confirmed_directory: None,
                        creator: Some(device),
                    }],
                });
                id
            }
            Mutation::SplitPane {
                workspace,
                tab,
                source,
                axis,
            } => {
                if self.pane_count() >= MAX_PANES {
                    return Ok(Outcome::Limit);
                }
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
                let start = w.pinned_directory.clone().unwrap_or_else(|| {
                    p.confirmed_directory
                        .as_ref()
                        .unwrap_or(&p.starting_directory)
                        .clone()
                });
                let id = new_id()?;
                t.panes.push(Pane {
                    id,
                    source: Some(*source),
                    axis: *axis,
                    starting_directory: start,
                    confirmed_directory: None,
                    creator: Some(device),
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
                w.name = name.trim().to_owned();
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
            if let Some(t) = w.tabs.first() {
                w.root_tab = t.id;
            }
        }
        self.inventory.workspaces.retain(|w| !w.tabs.is_empty());
        self.inventory.revision += 1;
    }
    pub fn repair_directory(&mut self, id: Id, path: PathBuf) -> Result<(), Refusal> {
        for workspace in &mut self.inventory.workspaces {
            for tab in &mut workspace.tabs {
                if let Some(pane) = tab.panes.iter_mut().find(|p| p.id == id) {
                    pane.confirmed_directory = Some(path);
                    tab.worktree_membership_fixed = true;
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
                        self.inventory.revision += 1;
                    }
                    return;
                }
            }
        }
    }
    pub(crate) fn validate(&self) -> Result<(), ServerError> {
        let invalid = || ServerError::UnsafeStorage;
        if self.inventory.workspaces.len() > MAX_WORKSPACES
            || self.tab_count() > MAX_TABS
            || self.pane_count() > MAX_PANES
            || self.devices.len() > 64
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
                if !o.mutation.is_bounded()
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
    pub result: Option<RetainedOutcome>,
}
#[cfg(feature = "client")]
#[derive(Default, Serialize, Deserialize)]
struct JournalRecord {
    pin: Id,
    device: Id,
    pending: Option<ClientOperation>,
    last: Option<RetainedOutcome>,
}
/// Pending shared edits live outside Settings and are fenced to their original authority.
#[cfg(feature = "client")]
pub struct ClientJournal {
    directory: crate::storage::Directory,
    _lock: std::fs::File,
    record: JournalRecord,
}
#[cfg(feature = "client")]
impl Drop for ClientJournal {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;
        // SAFETY: this guard owns the live lock descriptor. Explicit unlock ends its ownership
        // even when a concurrent fork inherited a descriptor that has not reached exec yet.
        unsafe {
            libc::flock(self._lock.as_raw_fd(), libc::LOCK_UN);
        }
    }
}
#[cfg(feature = "client")]
impl ClientJournal {
    pub(crate) fn open(
        directory: crate::storage::Directory,
        pin: Id,
        device: Id,
    ) -> Result<Self, ServerError> {
        let lock = directory.lock("operations.lock", true)?;
        let record = directory
            .read_json::<JournalRecord>("operations.json")?
            .unwrap_or(JournalRecord {
                pin,
                device,
                ..Default::default()
            });
        if record.pin != pin || record.device != device {
            return Err(ServerError::AuthenticationRefused);
        }
        Ok(Self {
            directory,
            _lock: lock,
            record,
        })
    }
    pub fn pending(&self) -> Option<&ClientOperation> {
        self.record.pending.as_ref()
    }
    pub fn last(&self) -> Option<&RetainedOutcome> {
        self.record.last.as_ref()
    }
    pub fn submit(&mut self, sequence: u64, mutation: Mutation) -> Result<(), ServerError> {
        if self.record.pending.is_some() {
            return Err(ServerError::Unsupported);
        }
        self.record.pending = Some(ClientOperation {
            sequence,
            mutation,
            result: None,
        });
        self.save()
    }
    pub fn complete(&mut self, result: RetainedOutcome) -> Result<(), ServerError> {
        let pending = self.record.pending.as_mut().ok_or(ServerError::Protocol)?;
        if pending.sequence != result.sequence || pending.mutation != result.mutation {
            return Err(ServerError::Protocol);
        }
        pending.result = Some(result);
        self.save()
    }
    pub fn retire(&mut self) -> Result<(), ServerError> {
        let result = self
            .record
            .pending
            .as_ref()
            .and_then(|p| p.result.clone())
            .ok_or(ServerError::Protocol)?;
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
    fn confirmed_close_promotes_roots_and_replay_cannot_remove_later_work() {
        let mut state = State::empty();
        state
            .apply(
                [1; 32],
                1,
                Mutation::CreateWorkspace {
                    name: "Build".into(),
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
                Mutation::CreateTab { workspace },
                "/tmp".as_ref(),
                |_| true,
            )
            .unwrap();
        let survivor = state.inventory.workspaces[0].tabs[1].id;
        let pane = state.inventory.workspaces[0].tabs[0].root_pane;
        state.inventory.close = Some(CloseInventory {
            boot: [9; 32],
            sessions: vec![ConfirmedPane {
                id: pane,
                session: None,
            }],
        });
        let close = Mutation::Close {
            target: CloseTarget::Tab { workspace, tab },
            confirmation: CloseConfirmation {
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
                Mutation::CreateTab { workspace },
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
    fn pending_outcomes_are_bounded_without_forgetting_unacknowledged_creations() {
        let mut state = State::empty();
        for sequence in 1..=32 {
            state
                .apply(
                    [1; 32],
                    sequence,
                    Mutation::CreateWorkspace {
                        name: "Build".into(),
                    },
                    "/tmp".as_ref(),
                    |_| true,
                )
                .unwrap();
        }
        let create = Mutation::CreateWorkspace {
            name: "More".into(),
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
    fn stale_pin_preserves_the_conflict_even_if_its_draft_directory_became_unavailable() {
        let mut state = State::empty();
        let Outcome::Accepted { target: workspace } = state
            .apply(
                [1; 32],
                1,
                Mutation::CreateWorkspace {
                    name: "Build".into(),
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
                },
                "/tmp".as_ref(),
                |_| true,
            )
            .unwrap();
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
        let mut restored = State::from_legacy(&reopened);
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
                    Mutation::CreateTab { workspace },
                    "/tmp".as_ref(),
                    |_| true
                )
                .unwrap(),
            Outcome::Missing
        );
        assert!(restored.inventory.workspaces.is_empty());
    }
}
