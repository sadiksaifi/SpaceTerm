//! Groups the Workspace Window's Workspaces by the machine they run on.
//!
//! The account-local server's Workspaces and this computer's own form the local group. Every other
//! machine is named by its SSH Destination, so an SSH server's Workspaces and the Remote Workspaces
//! opened at the same destination share one group. A server reached over a direct network route
//! forms its own group under the name it gave, apart from any SSH route to the same computer,
//! because the two routes carry separate authority. Workspace order keeps each group together,
//! the local one first, then the SSH machines and then the direct network servers by name, so the
//! sidebar, the position shortcuts, and the Workspace Switcher present one order.

use std::cmp::Ordering;

use gpui::{App, Context, Window};

use super::{ServerBindings, ServerOrigin, WorkspaceManager};
use crate::direct_connections::{DirectRouteId, DirectRouteStatus};
use crate::domain::{SshDestination, WorkspaceEntry, WorkspaceId, WorkspaceLocation};
use crate::ssh::server_connection::RemoteConnectionStatus;
use crate::ssh::server_operations::RemoteOperationState;
use crate::ui::remote_maintenance::{menu_actions_for, observed_menu_actions, working_state};
use crate::ui::workspace_sidebar::{MachineMenuCommand, MachineSection, MachineTarget};

/// What a direct network server's heading says beside its name.
const DIRECT_ROUTE: &str = "Direct network";

/// The machine whose group a Workspace joins.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Machine {
    Local,
    Ssh(SshDestination),
    /// A server reached over a direct network route, ordered by the name it gave.
    Direct {
        name: String,
        route: DirectRouteId,
    },
}

impl Machine {
    fn of<T>(servers: &ServerBindings, workspace: &WorkspaceEntry<T>) -> Self {
        if let Some(origin) = servers
            .owner(workspace.id())
            .and_then(|key| servers.origin(key))
        {
            return Self::from_origin(origin);
        }
        match workspace.location() {
            WorkspaceLocation::Local => Self::Local,
            WorkspaceLocation::Remote { key, .. } => Self::Ssh(key.destination().clone()),
        }
    }
}

impl Machine {
    fn from_origin(origin: &ServerOrigin) -> Self {
        match origin {
            ServerOrigin::Local => Self::Local,
            ServerOrigin::Ssh(destination) => Self::Ssh(destination.clone()),
            ServerOrigin::Direct(direct) => Self::Direct {
                name: direct.name.to_string(),
                route: direct.route,
            },
        }
    }

    const fn rank(&self) -> u8 {
        match self {
            Self::Local => 0,
            Self::Ssh(_) => 1,
            Self::Direct { .. } => 2,
        }
    }
}

/// Orders names ignoring case, so spellings that differ only in case stay distinct but adjacent.
fn by_name(a: &str, b: &str) -> Ordering {
    a.to_lowercase()
        .cmp(&b.to_lowercase())
        .then_with(|| a.cmp(b))
}

/// Group order: the local machine first, then each SSH machine by its destination, then each
/// direct network server by its name, ignoring case.
impl Ord for Machine {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Ssh(a), Self::Ssh(b)) => by_name(a.as_str(), b.as_str()),
            (
                Self::Direct {
                    name: a,
                    route: first,
                },
                Self::Direct {
                    name: b,
                    route: second,
                },
            ) => by_name(a, b).then_with(|| first.cmp(second)),
            _ => self.rank().cmp(&other.rank()),
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
        let presented = self
            .shared
            .keys()
            .into_iter()
            .filter_map(|key| self.shared.origin(key).map(Machine::from_origin));
        // A saved direct route keeps its heading while SpaceTerm tries it, waits for the person,
        // or was refused, so that state stays visible. A route the person disconnected has none.
        let direct = crate::ui::direct_servers::installed(cx);
        let direct_intended = direct
            .iter()
            .flat_map(|direct| {
                direct
                    .routes()
                    .into_iter()
                    .filter(|(_, status)| *status != DirectRouteStatus::Disconnected)
                    .filter_map(|(route, _)| {
                        Some(Machine::Direct {
                            name: direct.route(&route)?.label,
                            route,
                        })
                    })
            })
            .collect::<Vec<_>>();
        let mut ssh_server = false;
        for machine in presented
            .collect::<Vec<_>>()
            .into_iter()
            .chain(intended)
            .chain(direct_intended)
        {
            ssh_server |= !matches!(machine, Machine::Local);
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
                    route: None,
                    target: MachineTarget::Local,
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
                    // One destination can hold more than one unsettled original, so the
                    // heading reads every state the owner holds for it rather than the first.
                    let maintenance = operations.as_ref().filter(|_| saved).map(|operations| {
                        operations
                            .entries()
                            .into_iter()
                            .filter(|entry| !entry.removed && entry.destination == destination)
                            .map(|entry| entry.state)
                            .collect::<Vec<_>>()
                    });
                    MachineSection {
                        label: destination.as_str().to_owned().into(),
                        route: None,
                        // A Remote Workspace reports its own connection on its row. The heading
                        // reports the SSH server's, and an operation underway on that server
                        // outranks it: it is what is happening to the machine now.
                        status: maintenance
                            .as_ref()
                            .and_then(|states| {
                                states
                                    .iter()
                                    .find_map(|state| working_state(destination.as_str(), state))
                            })
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
                        maintenance: match &maintenance {
                            Some(states)
                                if states.iter().any(|state| {
                                    !matches!(state, RemoteOperationState::Unread)
                                }) =>
                            {
                                menu_actions_for(states, None)
                            }
                            // Before any maintenance read, the heading offers what the connection
                            // already observed about that account's server.
                            Some(_) => connections
                                .as_ref()
                                .map(|connections| {
                                    observed_menu_actions(&connections.status(&destination))
                                })
                                .unwrap_or_default(),
                            None => Vec::new(),
                        },
                        // Settings lists saved SSH connections; native Remote Workspaces alone
                        // name no server there.
                        has_settings: saved,
                        target: MachineTarget::Ssh(destination),
                        workspaces,
                    }
                }
                Machine::Direct { name, route } => {
                    let presented = self.direct_route_presented(&route);
                    let status = direct.as_ref().and_then(|direct| direct.status(&route));
                    let saved = status.is_some();
                    let intended = status.is_some_and(|status| {
                        !matches!(
                            status,
                            DirectRouteStatus::Disconnected | DirectRouteStatus::Refused(_)
                        )
                    });
                    let presentation =
                        status.map(|status| crate::ui::direct_servers::present(status, &name));
                    let connected = status == Some(DirectRouteStatus::Connected);
                    // Connect from the heading is the person asking, so it may let the system ask
                    // for this device's key, as Resume does.
                    let connects = presentation.as_ref().is_some_and(|presentation| {
                        presentation.actions.iter().any(|action| {
                            matches!(
                                action,
                                crate::ui::direct_servers::DirectServerAction::Connect
                                    | crate::ui::direct_servers::DirectServerAction::Resume
                            )
                        })
                    });
                    MachineSection {
                        label: name.into(),
                        route: Some(DIRECT_ROUTE),
                        target: MachineTarget::Direct(route),
                        status: presentation
                            .as_ref()
                            .filter(|_| !connected)
                            .map(|presentation| presentation.state),
                        connects,
                        disconnects: presented || intended,
                        has_settings: saved,
                        awaiting_connection: !presented,
                        // A direct network server's maintenance lives in its Settings detail.
                        maintenance: Vec::new(),
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
        target: MachineTarget,
        command: MachineMenuCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let destination = match target {
            MachineTarget::Ssh(destination) => Some(destination),
            MachineTarget::Direct(route) => {
                self.perform_direct_command(route, command, cx);
                return;
            }
            MachineTarget::Local => None,
        };
        match (command, destination) {
            (MachineMenuCommand::ServerSettings, destination) => {
                crate::ui::settings_window::open_server_settings(
                    destination.map_or(MachineTarget::Local, MachineTarget::Ssh),
                    cx,
                )
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

    /// Performs a direct network server heading's command. Connect lets the system ask for this
    /// device's key because the person asked; nothing here reaches the server over SSH.
    fn perform_direct_command(
        &mut self,
        route: DirectRouteId,
        command: MachineMenuCommand,
        cx: &mut Context<Self>,
    ) {
        match command {
            MachineMenuCommand::ServerSettings => {
                crate::ui::settings_window::open_server_settings(MachineTarget::Direct(route), cx)
            }
            MachineMenuCommand::Connect => {
                if let Some(direct) = crate::ui::direct_servers::installed(cx) {
                    direct.resume(&route);
                }
            }
            MachineMenuCommand::Disconnect => {
                if let Some(direct) = crate::ui::direct_servers::installed(cx) {
                    direct.disconnect(&route);
                }
            }
            MachineMenuCommand::Maintenance(_) => {}
        }
    }
}
