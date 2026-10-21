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
use crate::ui::remote_maintenance::{menu_actions, working_state};
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
        let connections = crate::ui::ssh_servers::installed(cx);
        // Reading the submitted operations reaches no computer, as reading a connection's status
        // does not.
        let operations = crate::ssh::server_operations::installed(cx);
        // A Saved Connection the person wants kept up has its heading before it connects, so a
        // connection that waits for sign-in after relaunch shows that state here.
        let intended = connections
            .iter()
            .flat_map(|connections| {
                connections
                    .connections()
                    .into_iter()
                    .map(|(destination, _)| destination)
                    .filter(|destination| connections.intends(destination))
            })
            .map(Machine::Ssh);
        let presented =
            self.shared
                .keys()
                .into_iter()
                .filter_map(|key| match self.shared.origin(key)? {
                    ServerOrigin::Local => Some(Machine::Local),
                    ServerOrigin::Ssh(destination) => Some(Machine::Ssh(destination.clone())),
                });
        let mut ssh_server = false;
        for machine in presented.collect::<Vec<_>>().into_iter().chain(intended) {
            ssh_server |= matches!(machine, Machine::Ssh(_));
            if let Err(position) = sections.binary_search_by(|(other, _)| other.cmp(&machine)) {
                sections.insert(position, (machine, 0));
            }
        }
        if sections.len() < 2 && !ssh_server {
            return Vec::new();
        }
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
                    has_settings: true,
                    awaiting_connection: false,
                    // The account-local server's own service and update controls live in its
                    // Settings detail, which this heading opens.
                    maintenance: Vec::new(),
                    workspaces,
                },
                Machine::Ssh(destination) => {
                    let presented = self
                        .shared
                        .key_of(&ServerOrigin::Ssh(destination.clone()))
                        .is_some();
                    // Settings lists saved SSH connections; native Remote Workspaces alone name
                    // no server there.
                    let saved = connections.as_ref().is_some_and(|connections| {
                        connections
                            .connections()
                            .iter()
                            .any(|(saved, _)| *saved == destination)
                    });
                    let intended = connections
                        .as_ref()
                        .is_some_and(|connections| connections.intends(&destination));
                    // A heading controls a connection SpaceTerm presents or keeps trying. A
                    // machine with only native Remote Workspaces offers no Connect that would set
                    // up a server there.
                    let controlled = presented || intended;
                    let presentation = connections.as_ref().map(|connections| {
                        let status = connections.status(&destination);
                        let connected = matches!(status, RemoteConnectionStatus::Connected { .. });
                        (connected, crate::ui::ssh_servers::present(&status))
                    });
                    let offers = |action| {
                        controlled
                            && presentation.as_ref().is_some_and(|(_, presentation)| {
                                presentation.actions.contains(&action)
                            })
                    };
                    // A saved connection's server is the one these steps act on, whether it is
                    // presented now or not.
                    let maintenance = operations
                        .as_ref()
                        .filter(|_| saved)
                        .map(|operations| operations.state(&destination));
                    MachineSection {
                        label: destination.as_str().to_owned().into(),
                        // A Remote Workspace reports its own connection on its row. The heading
                        // reports the SSH server's, and an operation underway on that server
                        // outranks it: it is what is happening to the machine now.
                        status: maintenance
                            .as_ref()
                            .and_then(|state| working_state(destination.as_str(), state))
                            .or_else(|| {
                                presentation
                                    .as_ref()
                                    .filter(|(connected, _)| controlled && !connected)
                                    .map(|(_, presentation)| presentation.state)
                            }),
                        connects: offers(crate::ui::ssh_servers::SshServerAction::Connect),
                        // Disconnect withdraws a presented server whatever its state, such as one
                        // stopped or failing after it connected, and ends reconnect intent.
                        disconnects: controlled,
                        awaiting_connection: !presented && intended,
                        maintenance: maintenance
                            .as_ref()
                            .map(|state| menu_actions(state, None))
                            .unwrap_or_default(),
                        // Settings lists saved SSH connections; native Remote Workspaces alone
                        // name no server there.
                        has_settings: saved,
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
            (MachineMenuCommand::Maintenance(action), Some(destination)) => {
                // Settings owns these steps, so one surface describes a change, asks about it and
                // keeps its result afterwards. The heading opens that surface on this machine.
                crate::ui::settings_window::open_remote_maintenance(destination, action, cx);
            }
            (
                MachineMenuCommand::Connect
                | MachineMenuCommand::Disconnect
                | MachineMenuCommand::Maintenance(_),
                None,
            ) => {}
        }
    }
}
