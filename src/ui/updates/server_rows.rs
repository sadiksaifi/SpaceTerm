//! Passive server inventory for the Updates dropdown. Routes identify servers; they do not
//! create maintenance authority. Only this computer's Settings journey prepares or activates an
//! update. A paired server is updated on its own computer, so its row only opens its Settings.

use std::collections::HashMap;

use gpui::{App, Context, SharedString, Subscription, Task};

use crate::application_identity::ApplicationIdentity;
use crate::direct_connections::{DirectConnections, DirectRouteStatus};
use crate::server_connection::ServerError;
use crate::server_connection::service::Service;
use crate::ui::MachineTarget;
use crate::ui::direct_servers::DirectChanges;
use crate::ui::settings_window;

/// Each status is a few words that fit the menu. The action's Settings journey explains it.
const UPDATE_STATUS_UNKNOWN: &str = "Status Unknown";
const UPDATE_NEEDS_ATTENTION: &str = "Update Needs Attention";

#[derive(Clone)]
pub(crate) struct ServerUpdateRow {
    pub label: SharedString,
    pub detail: SharedString,
    pub action: Option<ServerUpdateAction>,
}

/// An entry point into Settings, never an activation or an approval.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ServerUpdateAction {
    Local,
    /// The server's update stopped before it finished, and its Settings explain what to do.
    Review(MachineTarget),
    /// The server's Settings, which only report its status here.
    Settings(MachineTarget),
}

/// What a server asks of the user. Offline, unknown, and unauthorized status asks nothing.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum ServerNeed {
    Update,
    Attention,
}

impl ServerUpdateAction {
    pub(crate) fn label(&self) -> &'static str {
        match self {
            Self::Local => "Update Server…",
            Self::Review(_) | Self::Settings(_) => "Settings…",
        }
    }

    pub(crate) fn need(&self) -> Option<ServerNeed> {
        match self {
            Self::Local => Some(ServerNeed::Update),
            Self::Review(_) => Some(ServerNeed::Attention),
            Self::Settings(_) => None,
        }
    }

    pub(crate) fn activate(&self, cx: &mut App) {
        match self {
            Self::Local => settings_window::open_server_update(cx),
            Self::Review(target) | Self::Settings(target) => {
                settings_window::open_server_settings(target.clone(), cx)
            }
        }
    }
}

#[derive(Clone)]
struct Sources {
    direct: Option<DirectConnections>,
}

/// Owns background reads independently of the Settings Window. Rendering clones its snapshot.
pub(crate) struct ServerUpdates {
    sources: Sources,
    rows: Vec<ServerUpdateRow>,
    reading: bool,
    pending: bool,
    _read: Option<Task<()>>,
    _direct: Option<DirectChanges>,
    _local: Subscription,
}

impl ServerUpdates {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let local = settings_window::local_operations::LocalServerOperations::get(cx);
        let local = cx.subscribe(&local, |inventory, _, _, cx| inventory.refresh(cx));
        let sources = Sources {
            direct: crate::ui::direct_servers::installed(cx),
        };
        let direct = sources.direct.as_ref().map(|connections| {
            DirectChanges::follow(connections, |changes| {
                cx.spawn(async move |entity, cx| {
                    while changes.recv().await.is_ok() {
                        if entity
                            .update(cx, |inventory, cx| inventory.refresh(cx))
                            .is_err()
                        {
                            break;
                        }
                    }
                })
            })
        });
        let mut inventory = Self {
            sources,
            rows: vec![ServerUpdateRow {
                label: "This computer".into(),
                detail: "Reading Update Status…".into(),
                action: Some(ServerUpdateAction::Settings(MachineTarget::Local)),
            }],
            reading: false,
            pending: false,
            _read: None,
            _direct: direct,
            _local: local,
        };
        inventory.refresh(cx);
        inventory
    }

    pub(crate) fn rows(&self) -> Vec<ServerUpdateRow> {
        self.rows.clone()
    }

    /// Also called when the dropdown opens, so a local service change is observed without polling.
    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.reading {
            self.pending = true;
            return;
        }
        self.reading = true;
        let sources = self.sources.clone();
        let read = cx
            .background_executor()
            .spawn(async move { read_inventory(sources) });
        self._read = Some(cx.spawn(async move |entity, cx| {
            let rows = read.await;
            let _ = entity.update(cx, |inventory, cx| {
                inventory.rows = rows;
                inventory.reading = false;
                cx.notify();
                if std::mem::take(&mut inventory.pending) {
                    inventory.refresh(cx);
                }
            });
        }));
    }
}

#[derive(Clone, Eq, PartialEq, Hash)]
enum ServerKey {
    Known { build: String, server: [u8; 32] },
    UnknownLocal,
}

struct Observation {
    key: ServerKey,
    row: ServerUpdateRow,
    /// Prefer an available update's authorized route, then local authority, then a paired route.
    route_priority: u8,
}

fn read_inventory(sources: Sources) -> Vec<ServerUpdateRow> {
    let identity = ApplicationIdentity::current().directory_name();
    let service = Service::for_identity(identity);
    let key = service
        .as_ref()
        .ok()
        .and_then(|service| service.passive_server_id().ok().flatten())
        .map_or(ServerKey::UnknownLocal, |server| ServerKey::Known {
            build: identity.to_owned(),
            server,
        });
    let row = if crate::local_startup::owned_foreground() {
        local_row(Ok(("Development Server".into(), false)))
    } else {
        local_row(
            service
                .as_ref()
                .map_err(|error| *error)
                .and_then(settings_window::server_update::passive_update),
        )
    };
    let local = Observation {
        key,
        row,
        route_priority: 2,
    };
    let mut observations = vec![local];
    if let Some(direct) = sources.direct {
        for (id, status) in direct.routes() {
            let Some(route) = direct.route(&id) else {
                continue;
            };
            let state = if status == DirectRouteStatus::Connected {
                "Connected"
            } else {
                "Not Connected"
            };
            observations.push(Observation {
                key: ServerKey::Known {
                    build: route.paired.pin.build,
                    server: route.paired.pin.server,
                },
                row: ServerUpdateRow {
                    label: route.label.into(),
                    // A paired server is updated on its own computer.
                    detail: format!("{state} · {UPDATE_STATUS_UNKNOWN}").into(),
                    action: Some(ServerUpdateAction::Settings(MachineTarget::Direct(id))),
                },
                route_priority: 1,
            });
        }
    }
    merge_rows(observations)
}

/// The account server's row from its passive read. A passive read refuses an unresolved update,
/// which is the one refusal that asks the user to act.
fn local_row(passive: Result<(SharedString, bool), ServerError>) -> ServerUpdateRow {
    let (detail, action) = match passive {
        Ok((detail, true)) => (detail, ServerUpdateAction::Local),
        Ok((detail, false)) => (detail, ServerUpdateAction::Settings(MachineTarget::Local)),
        Err(ServerError::ActivationUnresolved) => (
            UPDATE_NEEDS_ATTENTION.into(),
            ServerUpdateAction::Review(MachineTarget::Local),
        ),
        Err(_) => (
            UPDATE_STATUS_UNKNOWN.into(),
            ServerUpdateAction::Settings(MachineTarget::Local),
        ),
    };
    ServerUpdateRow {
        label: "This computer".into(),
        detail,
        action: Some(action),
    }
}

fn merge_rows(observations: Vec<Observation>) -> Vec<ServerUpdateRow> {
    let mut positions = HashMap::new();
    let mut servers: Vec<Observation> = Vec::new();
    // An available update's route wins, then a route that reports a stopped update.
    let priority = |observation: &Observation| {
        let need = observation
            .row
            .action
            .as_ref()
            .and_then(ServerUpdateAction::need);
        let rank = match need {
            Some(ServerNeed::Update) => 2,
            Some(ServerNeed::Attention) => 1,
            None => 0,
        };
        (rank, observation.route_priority)
    };
    for observation in observations {
        if let Some(index) = positions.get(&observation.key).copied() {
            if priority(&observation) > priority(&servers[index]) {
                servers[index] = observation;
            }
        } else {
            positions.insert(observation.key.clone(), servers.len());
            servers.push(observation);
        }
    }
    servers.into_iter().map(|server| server.row).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stopped_local_update_needs_attention_while_unknown_status_asks_nothing() {
        let unresolved = local_row(Err(ServerError::ActivationUnresolved));
        assert_eq!(&*unresolved.detail, "Update Needs Attention");
        assert_eq!(
            unresolved.action.unwrap().need(),
            Some(ServerNeed::Attention)
        );
        assert_eq!(
            local_row(Ok(("Update Available".into(), true)))
                .action
                .unwrap()
                .need(),
            Some(ServerNeed::Update)
        );
        let unknown = local_row(Err(ServerError::NotSetUp));
        assert_eq!(&*unknown.detail, UPDATE_STATUS_UNKNOWN);
        assert_eq!(unknown.action.unwrap().need(), None);
    }

    #[test]
    fn a_paired_route_to_this_computers_server_merges_into_its_local_row() {
        let key = ServerKey::Known {
            build: "spaceterm-preflight".into(),
            server: [3; 32],
        };
        let observed = |key, action, priority| Observation {
            key,
            row: ServerUpdateRow {
                label: "machine".into(),
                detail: "unknown".into(),
                action: Some(action),
            },
            route_priority: priority,
        };
        let rows = merge_rows(vec![
            observed(
                key.clone(),
                ServerUpdateAction::Settings(MachineTarget::Direct([7; 32])),
                1,
            ),
            observed(key, ServerUpdateAction::Local, 2),
            observed(
                ServerKey::Known {
                    build: "spaceterm-preflight".into(),
                    server: [4; 32],
                },
                ServerUpdateAction::Settings(MachineTarget::Direct([8; 32])),
                1,
            ),
        ]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].action, Some(ServerUpdateAction::Local));
        assert_eq!(
            rows[1].action,
            Some(ServerUpdateAction::Settings(MachineTarget::Direct([8; 32])))
        );
    }
}
