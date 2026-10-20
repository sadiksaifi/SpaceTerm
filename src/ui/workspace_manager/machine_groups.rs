//! Groups the Workspace Window's Workspaces by the machine they run on.
//!
//! The account-local server's Workspaces and this computer's own form the local group. Every other
//! machine is named by its SSH Destination, so an SSH server's Workspaces and the Remote Workspaces
//! opened at the same destination share one group. Workspace order keeps each group together, the
//! local one first and then the others by name, so the sidebar, the position shortcuts, and the
//! Workspace Switcher present one order.

use std::cmp::Ordering;

use super::{ServerBindings, ServerOrigin, WorkspaceManager};
use crate::domain::{SshDestination, WorkspaceEntry, WorkspaceId, WorkspaceLocation};

/// The machine whose group a Workspace joins.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Machine {
    Local,
    Ssh(SshDestination),
}

impl Machine {
    fn of<T>(servers: &ServerBindings, workspace: &WorkspaceEntry<T>) -> Self {
        if let Some(origin) = servers
            .owner(workspace.id())
            .and_then(|key| servers.origin(key))
        {
            return match origin {
                ServerOrigin::Local => Self::Local,
                ServerOrigin::Ssh(destination) => Self::Ssh(destination.clone()),
            };
        }
        match workspace.location() {
            WorkspaceLocation::Local => Self::Local,
            WorkspaceLocation::Remote { key, .. } => Self::Ssh(key.destination().clone()),
        }
    }
}

/// Group order: the local machine first, then each other machine by its destination, ignoring
/// case. Spellings that differ only in case stay distinct machines.
impl Ord for Machine {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Local, Self::Local) => Ordering::Equal,
            (Self::Local, Self::Ssh(_)) => Ordering::Less,
            (Self::Ssh(_), Self::Local) => Ordering::Greater,
            (Self::Ssh(a), Self::Ssh(b)) => a
                .as_str()
                .to_lowercase()
                .cmp(&b.as_str().to_lowercase())
                .then_with(|| a.as_str().cmp(b.as_str())),
        }
    }
}

impl PartialOrd for Machine {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl WorkspaceManager {
    /// The machine whose group the Workspace joins, or `None` for a Workspace this window does not
    /// present.
    pub(super) fn machine(&self, workspace_id: WorkspaceId) -> Option<Machine> {
        self.workspaces
            .workspace(workspace_id)
            .map(|workspace| Machine::of(&self.shared, workspace))
    }

    /// Restores grouped Workspace order after a Workspace joins, so it ends its machine's group.
    pub(super) fn arrange_by_machine(&mut self) {
        let servers = &self.shared;
        self.workspaces
            .arrange_by(|workspace| Machine::of(servers, workspace));
    }

    /// The position nearest `position` among the Workspaces of `workspace_id`'s machine, where a
    /// move of that Workspace lands.
    pub(super) fn position_within_machine(
        &self,
        workspace_id: WorkspaceId,
        position: usize,
    ) -> usize {
        let Some(machine) = self.machine(workspace_id) else {
            return position;
        };
        let mut positions = self
            .workspaces
            .iter()
            .enumerate()
            .filter(|(_, workspace)| Machine::of(&self.shared, workspace) == machine)
            .map(|(position, _)| position);
        let Some(first) = positions.next() else {
            return position;
        };
        let last = positions.last().unwrap_or(first);
        position.clamp(first, last)
    }
}
