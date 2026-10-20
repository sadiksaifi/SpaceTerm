//! Groups the Workspace Window's Workspaces by the machine they run on.
//!
//! The account-local server's Workspaces and this computer's own form the local group. Every other
//! machine is named by its SSH Destination, so an SSH server's Workspaces and the Remote Workspaces
//! opened at the same destination share one group. Workspace order keeps each group together, the
//! local one first and then the others by name, so the sidebar, the position shortcuts, and the
//! Workspace Switcher present one order.

use std::cmp::Ordering;

use gpui::{App, Context, Window};

use super::{ServerBindings, ServerOrigin, WorkspaceManager};
use crate::domain::{SshDestination, WorkspaceEntry, WorkspaceId, WorkspaceLocation};
use crate::ssh::server_connection::RemoteConnectionStatus;
use crate::ui::workspace_sidebar::{MachineMenuCommand, MachineSection};

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

    /// The sidebar's machine headings in Workspace order, or none while the Workspaces of one
    /// machine need no heading. Every presented server has its heading, even with no Workspaces,
    /// so an empty or disconnected server stays visible. Reading the SSH servers' statuses
    /// performs no remote work.
    pub(super) fn machine_sections(&self, cx: &App) -> Vec<MachineSection> {
        let mut sections: Vec<(Machine, usize)> = Vec::new();
        for workspace in self.workspaces.iter() {
            let machine = Machine::of(&self.shared, workspace);
            match sections.last_mut() {
                Some((last, count)) if *last == machine => *count += 1,
                _ => sections.push((machine, 1)),
            }
        }
        for key in self.shared.keys() {
            let machine = match self.shared.origin(key) {
                Some(ServerOrigin::Local) => Machine::Local,
                Some(ServerOrigin::Ssh(destination)) => Machine::Ssh(destination.clone()),
                None => continue,
            };
            if let Err(position) = sections.binary_search_by(|(other, _)| other.cmp(&machine)) {
                sections.insert(position, (machine, 0));
            }
        }
        let ssh_server = self
            .shared
            .keys()
            .into_iter()
            .any(|key| matches!(self.shared.origin(key), Some(ServerOrigin::Ssh(_))));
        if sections.len() < 2 && !ssh_server {
            return Vec::new();
        }
        let connections = crate::ui::ssh_servers::installed(cx);
        let this_computer = crate::desktop_profile::DesktopPresentation::get(cx)
            .wording()
            .this_computer;
        sections
            .into_iter()
            .map(|(machine, workspaces)| match machine {
                Machine::Local => MachineSection {
                    label: this_computer.into(),
                    destination: None,
                    status: None,
                    connects: false,
                    disconnects: false,
                    workspaces,
                },
                Machine::Ssh(destination) => {
                    let presented = self
                        .shared
                        .key_of(&ServerOrigin::Ssh(destination.clone()))
                        .is_some();
                    let presentation = connections.as_ref().map(|connections| {
                        let status = connections.status(&destination);
                        let connected = matches!(status, RemoteConnectionStatus::Connected { .. });
                        (connected, crate::ui::ssh_servers::present(&status))
                    });
                    // Only a presented SSH server's heading controls its connection. A machine
                    // with only native Remote Workspaces offers no Connect that would set up a
                    // server there.
                    let offers = |action| {
                        presented
                            && presentation.as_ref().is_some_and(|(_, presentation)| {
                                presentation.actions.contains(&action)
                            })
                    };
                    MachineSection {
                        label: destination.as_str().to_owned().into(),
                        // A Remote Workspace reports its own connection on its row. The heading
                        // reports the SSH server's.
                        status: presentation
                            .as_ref()
                            .filter(|(connected, _)| presented && !connected)
                            .map(|(_, presentation)| presentation.state),
                        connects: offers(crate::ui::ssh_servers::SshServerAction::Connect),
                        disconnects: offers(crate::ui::ssh_servers::SshServerAction::Disconnect),
                        destination: Some(destination),
                        workspaces,
                    }
                }
            })
            .collect()
    }

    /// Performs a machine heading's command. Connecting is the person asking here, so any
    /// sign-in or host approval it needs appears in this window.
    pub(super) fn perform_machine_command(
        &mut self,
        destination: Option<SshDestination>,
        command: MachineMenuCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match (command, destination) {
            (MachineMenuCommand::ServerSettings, destination) => {
                crate::ui::settings_window::open_server_settings(destination, cx)
            }
            (MachineMenuCommand::Connect, Some(destination)) => {
                if let Some(connections) = crate::ui::ssh_servers::installed(cx) {
                    crate::ui::ssh_servers::present_prompts_in(window, cx);
                    connections.connect(destination);
                }
            }
            (MachineMenuCommand::Disconnect, Some(destination)) => {
                if let Some(connections) = crate::ui::ssh_servers::installed(cx) {
                    connections.disconnect(&destination);
                }
            }
            (MachineMenuCommand::Connect | MachineMenuCommand::Disconnect, None) => {}
        }
    }
}
