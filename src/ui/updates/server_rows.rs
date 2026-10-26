//! Passive server inventory for the Updates dropdown. Routes identify servers; they do not
//! create maintenance authority. Only this computer's Settings journey prepares or activates an
//! update. A paired server is updated on its own computer, so its row only opens its Settings.

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{App, Context, Global, SharedString, Subscription, Task};

use crate::application_identity::ApplicationIdentity;
use crate::direct_connections::{DirectConnections, DirectRouteStatus};
use crate::server_connection::ServerError;
use crate::ui::ServerTarget;
use crate::ui::direct_servers::DirectChanges;
use crate::ui::settings_window;

/// Each status is a few words that fit the menu. The action's Settings journey explains it.
const UPDATE_STATUS_UNKNOWN: &str = "Status Unknown";
const UPDATE_NEEDS_ATTENTION: &str = "Update Needs Attention";

/// What a passive read of this computer's server reported about updating it.
#[derive(Clone, Debug)]
pub(crate) struct LocalUpdateFacts {
    /// The server's identity, when its service reports one. Rows for one server merge by it.
    pub(crate) server: Option<[u8; 32]>,
    /// The update status as the Updates menu says it, and whether Update Server is offered, or
    /// why the read failed.
    pub(crate) update: Result<(SharedString, bool), ServerError>,
}

/// Reads this computer's server through its operating-system service. A read is passive: it
/// prepares, activates and recovers nothing. Reading blocks, so it runs on a background worker.
pub(crate) trait LocalUpdateObservation: Send + Sync {
    fn observe(&self) -> LocalUpdateFacts;
}

struct InstalledLocalUpdates(Arc<dyn LocalUpdateObservation>);
impl Global for InstalledLocalUpdates {}

/// Makes `observation` the one the Updates menu reads this computer's server through. The host
/// composition installs it before any Workspace Window opens.
pub(crate) fn install_local_updates(observation: Arc<dyn LocalUpdateObservation>, cx: &mut App) {
    cx.set_global(InstalledLocalUpdates(observation));
}

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
    Review(ServerTarget),
    /// The server's Settings, which only report its status here.
    Settings(ServerTarget),
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
    local: Arc<dyn LocalUpdateObservation>,
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
    /// The inventory read through the installed local observation, which the host composition
    /// provides.
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let local = settings_window::local_operations::LocalServerOperations::get(cx);
        let local = cx.subscribe(&local, |inventory, _, _, cx| inventory.refresh(cx));
        let sources = Sources {
            local: Arc::clone(&cx.global::<InstalledLocalUpdates>().0),
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
                action: Some(ServerUpdateAction::Settings(ServerTarget::Local)),
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
    let facts = sources.local.observe();
    let key = facts
        .server
        .map_or(ServerKey::UnknownLocal, |server| ServerKey::Known {
            build: identity.to_owned(),
            server,
        });
    let local = Observation {
        key,
        row: local_row(facts.update),
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
                    action: Some(ServerUpdateAction::Settings(ServerTarget::Direct(id))),
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
        Ok((detail, false)) => (detail, ServerUpdateAction::Settings(ServerTarget::Local)),
        Err(ServerError::ActivationUnresolved) => (
            UPDATE_NEEDS_ATTENTION.into(),
            ServerUpdateAction::Review(ServerTarget::Local),
        ),
        Err(_) => (
            UPDATE_STATUS_UNKNOWN.into(),
            ServerUpdateAction::Settings(ServerTarget::Local),
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

/// Scripted local observations for tests that open Workspace Windows or the Updates menu.
#[cfg(test)]
pub(crate) mod testing {
    use std::sync::Arc;

    use gpui::{App, SharedString};

    use super::{LocalUpdateFacts, LocalUpdateObservation, install_local_updates};

    /// This computer's server as a test scripts it. Every read reports the same facts.
    pub(crate) struct ScriptedLocalUpdates(LocalUpdateFacts);

    impl LocalUpdateObservation for ScriptedLocalUpdates {
        fn observe(&self) -> LocalUpdateFacts {
            self.0.clone()
        }
    }

    /// Installs a local server that reports `status`, offering Update Server when `offered`.
    pub(crate) fn install(status: &'static str, offered: bool, cx: &mut App) {
        let scripted = ScriptedLocalUpdates(LocalUpdateFacts {
            server: None,
            update: Ok((SharedString::new_static(status), offered)),
        });
        install_local_updates(Arc::new(scripted), cx);
    }

    /// Installs a local server that is up to date, for tests about anything but its update.
    pub(crate) fn install_up_to_date(cx: &mut App) {
        install("Up to Date", false, cx);
    }
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
                ServerUpdateAction::Settings(ServerTarget::Direct([7; 32])),
                1,
            ),
            observed(key, ServerUpdateAction::Local, 2),
            observed(
                ServerKey::Known {
                    build: "spaceterm-preflight".into(),
                    server: [4; 32],
                },
                ServerUpdateAction::Settings(ServerTarget::Direct([8; 32])),
                1,
            ),
        ]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].action, Some(ServerUpdateAction::Local));
        assert_eq!(
            rows[1].action,
            Some(ServerUpdateAction::Settings(ServerTarget::Direct([8; 32])))
        );
    }
}
