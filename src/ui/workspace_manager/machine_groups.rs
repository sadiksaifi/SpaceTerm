//! Groups the Workspace Window's Workspaces by the machine they run on.
//!
//! The account-local server's Workspaces form the local group. Each paired server forms its own
//! group under the name it gave. Workspace order keeps each group together, the local one first,
//! then the paired servers by name, so the sidebar, the position shortcuts, and the Workspace
//! Switcher present one order.

use std::cmp::Ordering;

use gpui::{App, Context, Window};

use super::{ServerBindings, ServerOrigin, WorkspaceManager};
use crate::direct_connections::{DirectRouteId, DirectRouteStatus};
use crate::domain::{WorkspaceEntry, WorkspaceId};
use crate::ui::workspace_sidebar::{MachineMenuCommand, MachineSection, MachineTarget};

/// The machine whose group a Workspace joins.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Machine {
    Local,
    /// A server reached over a paired route, ordered by the name it gave.
    Direct {
        name: String,
        route: DirectRouteId,
    },
}

impl Machine {
    fn of<T>(servers: &ServerBindings, workspace: &WorkspaceEntry<T>) -> Self {
        servers
            .owner(workspace.id())
            .and_then(|key| servers.origin(key))
            .map_or(Self::Local, Self::from_origin)
    }
}

impl Machine {
    fn from_origin(origin: &ServerOrigin) -> Self {
        match origin {
            ServerOrigin::Local => Self::Local,
            ServerOrigin::Direct(direct) => Self::Direct {
                name: direct.name.to_string(),
                route: direct.route,
            },
        }
    }

    const fn rank(&self) -> u8 {
        match self {
            Self::Local => 0,
            Self::Direct { .. } => 1,
        }
    }
}

/// Orders names ignoring case, so spellings that differ only in case stay distinct but adjacent.
fn by_name(a: &str, b: &str) -> Ordering {
    a.to_lowercase()
        .cmp(&b.to_lowercase())
        .then_with(|| a.cmp(b))
}

/// Group order: the local machine first, then each paired server by its name, ignoring case.
impl Ord for Machine {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
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
    /// so an empty or disconnected server stays visible. Reading the paired routes' statuses
    /// performs no network work.
    pub(super) fn machine_sections(&self, cx: &App) -> Vec<MachineSection> {
        let mut sections: Vec<(Machine, usize)> = Vec::new();
        for workspace in self.workspaces.iter() {
            let machine = Machine::of(&self.shared, workspace);
            match sections.last_mut() {
                Some((last, count)) if *last == machine => *count += 1,
                _ => sections.push((machine, 1)),
            }
        }
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
        let mut remote_server = false;
        for machine in presented
            .collect::<Vec<_>>()
            .into_iter()
            .chain(direct_intended)
        {
            remote_server |= !matches!(machine, Machine::Local);
            if let Err(position) = sections.binary_search_by(|(other, _)| other.cmp(&machine)) {
                sections.insert(position, (machine, 0));
            }
        }
        if sections.len() < 2 && !remote_server {
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
                    target: MachineTarget::Local,
                    status: None,
                    connects: false,
                    disconnects: false,
                    has_settings: true,
                    awaiting_connection: false,
                    // The footer's New Workspace already creates on this computer's server.
                    creates_workspaces: false,
                    workspaces,
                },
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
                        target: MachineTarget::Direct(route),
                        status: presentation
                            .as_ref()
                            .filter(|_| !connected)
                            .map(|presentation| presentation.state),
                        connects,
                        disconnects: presented || intended,
                        has_settings: saved,
                        awaiting_connection: !presented,
                        creates_workspaces: presented,
                        workspaces,
                    }
                }
            })
            .collect()
    }

    /// Performs a machine heading's command. Connect lets the system ask for this device's key
    /// because the person asked.
    pub(super) fn perform_machine_command(
        &mut self,
        target: MachineTarget,
        command: MachineMenuCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match (command, target) {
            (MachineMenuCommand::NewWorkspace, target) => {
                self.create_workspace_on(&target, String::new(), window, cx);
            }
            (MachineMenuCommand::ServerSettings, target) => {
                crate::ui::settings_window::open_server_settings(target, cx)
            }
            (MachineMenuCommand::Connect, MachineTarget::Direct(route)) => {
                if let Some(direct) = crate::ui::direct_servers::installed(cx) {
                    direct.resume(&route);
                }
            }
            (MachineMenuCommand::Disconnect, MachineTarget::Direct(route)) => {
                if let Some(direct) = crate::ui::direct_servers::installed(cx) {
                    direct.disconnect(&route);
                }
            }
            (
                MachineMenuCommand::Connect | MachineMenuCommand::Disconnect,
                MachineTarget::Local,
            ) => {}
        }
    }

    /// Asks the server `target` names for a new Workspace named `name` unless blank, which it
    /// presents to every connected client and this window activates once the server publishes
    /// it.
    pub(super) fn create_workspace_on(
        &mut self,
        target: &MachineTarget,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::ui::shared_workspaces::{SharedRefusalReason, SharedSubmission};

        let Some(shared) = self
            .server_key(target)
            .and_then(|key| self.shared.get_mut(key))
        else {
            super::WorkspaceManager::show_shared_refusal(
                "create a Workspace",
                SharedRefusalReason::Offline,
                window,
                cx,
            );
            return;
        };
        if let SharedSubmission::Refused(reason) = shared.create_workspace(name, None) {
            super::WorkspaceManager::show_shared_refusal("create a Workspace", reason, window, cx);
        }
        cx.notify();
    }

    /// Every saved paired server the creation menus offer, by name. Only one this window
    /// presents can take a new Workspace now.
    pub(super) fn remote_creation_servers(
        &self,
        cx: &App,
    ) -> Vec<crate::ui::workspace_creation::RemoteCreationServer> {
        let Some(direct) = crate::ui::direct_servers::installed(cx) else {
            return Vec::new();
        };
        let mut servers: Vec<_> = direct
            .routes()
            .into_iter()
            .filter_map(|(route, _)| {
                Some(crate::ui::workspace_creation::RemoteCreationServer {
                    route,
                    name: direct.route(&route)?.label.into(),
                    available: self.direct_route_presented(&route),
                })
            })
            .collect();
        servers.sort_by(|a, b| by_name(&a.name, &b.name).then_with(|| a.route.cmp(&b.route)));
        servers
    }

    /// The binding presenting `target`'s server, if this window presents it.
    pub(super) fn server_key(&self, target: &MachineTarget) -> Option<super::ServerKey> {
        self.shared
            .keys()
            .into_iter()
            .find(|key| match (self.shared.origin(*key), target) {
                (Some(ServerOrigin::Local), MachineTarget::Local) => true,
                (Some(ServerOrigin::Direct(bound)), MachineTarget::Direct(route)) => {
                    bound.route == *route
                }
                _ => false,
            })
    }
}
