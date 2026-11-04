//! Passive server inventory for the Updates dropdown. Routes identify servers; they do not
//! create maintenance authority. Only this computer's Settings journey prepares or activates an
//! update. A paired server is updated on its own computer, so its row only opens its Settings.

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{App, Context, Global, SharedString, Subscription, Task};

use crate::application_identity::ApplicationIdentity;
use crate::direct_connections::{DirectConnections, DirectRouteStatus};
use crate::machine_connection::CoreError;
use crate::ui::MachineTarget;
use crate::ui::direct_machines::DirectChanges;
use crate::ui::settings_window;

/// Each status is a few words that fit the menu. The action's Settings journey explains it.
const UPDATE_STATUS_UNKNOWN: &str = "Status unknown";
const UPDATE_NEEDS_ATTENTION: &str = "Update needs attention";

/// What a passive read of this computer's server reported about updating it.
#[derive(Clone, Debug)]
pub(crate) struct LocalUpdateFacts {
    /// The server's identity, when its service reports one. Rows for one server merge by it.
    pub(crate) core: Option<[u8; 32]>,
    /// The update status as the Updates menu says it, and whether Update SpaceTerm Core is offered, or
    /// why the read failed.
    pub(crate) update: Result<(SharedString, bool), CoreError>,
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
pub(crate) struct MachineUpdateRow {
    pub label: SharedString,
    pub detail: SharedString,
    pub action: Option<CoreUpdateAction>,
}

/// An entry point into Settings, never an activation or an approval.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CoreUpdateAction {
    Local,
    /// The server's update stopped before it finished, and its Settings explain what to do.
    Review(MachineTarget),
    /// The server's Settings, which only report its status here.
    Settings(MachineTarget),
}

/// What a server asks of the user. Offline, unknown, and unauthorized status asks nothing.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum CoreNeed {
    Update,
    Attention,
}

impl CoreUpdateAction {
    pub(crate) fn label(&self) -> &'static str {
        match self {
            Self::Local => "Update SpaceTerm Core…",
            Self::Review(_) | Self::Settings(_) => "Settings…",
        }
    }

    pub(crate) fn need(&self) -> Option<CoreNeed> {
        match self {
            Self::Local => Some(CoreNeed::Update),
            Self::Review(_) => Some(CoreNeed::Attention),
            Self::Settings(_) => None,
        }
    }

    pub(crate) fn activate(&self, cx: &mut App) {
        match self {
            Self::Local => settings_window::open_core_update(cx),
            Self::Review(target) | Self::Settings(target) => {
                settings_window::open_machine_settings(target.clone(), cx)
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
pub(crate) struct CoreUpdates {
    sources: Sources,
    rows: Vec<MachineUpdateRow>,
    reading: bool,
    pending: bool,
    _read: Option<Task<()>>,
    _direct: Option<DirectChanges>,
    _local: Subscription,
}

impl CoreUpdates {
    /// The inventory read through the installed local observation, which the host composition
    /// provides.
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let local = settings_window::local_operations::LocalCoreOperations::get(cx);
        let local = cx.subscribe(&local, |inventory, _, _, cx| inventory.refresh(cx));
        let sources = Sources {
            local: Arc::clone(&cx.global::<InstalledLocalUpdates>().0),
            direct: crate::ui::direct_machines::installed(cx),
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
            rows: vec![MachineUpdateRow {
                label: crate::machine_connection::service::control::this_computer().into(),
                detail: "Reading Update Status…".into(),
                action: Some(CoreUpdateAction::Settings(MachineTarget::Local)),
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

    pub(crate) fn rows(&self) -> Vec<MachineUpdateRow> {
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
enum MachineKey {
    Known { build: String, core: [u8; 32] },
    UnknownLocal,
}

struct Observation {
    key: MachineKey,
    row: MachineUpdateRow,
    /// Prefer an available update's authorized route, then local authority, then a paired route.
    route_priority: u8,
}

fn read_inventory(sources: Sources) -> Vec<MachineUpdateRow> {
    let identity = ApplicationIdentity::current().directory_name();
    let facts = sources.local.observe();
    let key = facts
        .core
        .map_or(MachineKey::UnknownLocal, |core| MachineKey::Known {
            build: identity.to_owned(),
            core,
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
                "Not connected"
            };
            observations.push(Observation {
                key: MachineKey::Known {
                    build: route.paired.pin.build,
                    core: route.paired.pin.core,
                },
                row: MachineUpdateRow {
                    label: route.label.into(),
                    // A paired server is updated on its own computer.
                    detail: format!("{state} · {UPDATE_STATUS_UNKNOWN}").into(),
                    action: Some(CoreUpdateAction::Settings(MachineTarget::Direct(id))),
                },
                route_priority: 1,
            });
        }
    }
    merge_rows(observations)
}

/// The account server's row from its passive read. A passive read refuses an unresolved update,
/// which is the one refusal that asks the user to act.
fn local_row(passive: Result<(SharedString, bool), CoreError>) -> MachineUpdateRow {
    let (detail, action) = match passive {
        Ok((detail, true)) => (detail, CoreUpdateAction::Local),
        Ok((detail, false)) => (detail, CoreUpdateAction::Settings(MachineTarget::Local)),
        Err(CoreError::ActivationUnresolved) => (
            UPDATE_NEEDS_ATTENTION.into(),
            CoreUpdateAction::Review(MachineTarget::Local),
        ),
        Err(_) => (
            UPDATE_STATUS_UNKNOWN.into(),
            CoreUpdateAction::Settings(MachineTarget::Local),
        ),
    };
    MachineUpdateRow {
        label: crate::machine_connection::service::control::this_computer().into(),
        detail,
        action: Some(action),
    }
}

fn merge_rows(observations: Vec<Observation>) -> Vec<MachineUpdateRow> {
    let mut positions = HashMap::new();
    let mut machines: Vec<Observation> = Vec::new();
    // An available update's route wins, then a route that reports a stopped update.
    let priority = |observation: &Observation| {
        let need = observation
            .row
            .action
            .as_ref()
            .and_then(CoreUpdateAction::need);
        let rank = match need {
            Some(CoreNeed::Update) => 2,
            Some(CoreNeed::Attention) => 1,
            None => 0,
        };
        (rank, observation.route_priority)
    };
    for observation in observations {
        if let Some(index) = positions.get(&observation.key).copied() {
            if priority(&observation) > priority(&machines[index]) {
                machines[index] = observation;
            }
        } else {
            positions.insert(observation.key.clone(), machines.len());
            machines.push(observation);
        }
    }
    machines.into_iter().map(|core| core.row).collect()
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

    /// Installs a local server that reports `status`, offering Update SpaceTerm Core when `offered`.
    pub(crate) fn install(status: &'static str, offered: bool, cx: &mut App) {
        let scripted = ScriptedLocalUpdates(LocalUpdateFacts {
            core: None,
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
        let unresolved = local_row(Err(CoreError::ActivationUnresolved));
        assert_eq!(&*unresolved.detail, "Update needs attention");
        assert_eq!(unresolved.action.unwrap().need(), Some(CoreNeed::Attention));
        assert_eq!(
            local_row(Ok(("Update Available".into(), true)))
                .action
                .unwrap()
                .need(),
            Some(CoreNeed::Update)
        );
        let unknown = local_row(Err(CoreError::NotSetUp));
        assert_eq!(&*unknown.detail, UPDATE_STATUS_UNKNOWN);
        assert_eq!(unknown.action.unwrap().need(), None);
    }

    #[test]
    fn a_paired_route_to_this_computers_machine_merges_into_its_local_row() {
        let key = MachineKey::Known {
            build: "spaceterm-preflight".into(),
            core: [3; 32],
        };
        let observed = |key, action, priority| Observation {
            key,
            row: MachineUpdateRow {
                label: "machine".into(),
                detail: "unknown".into(),
                action: Some(action),
            },
            route_priority: priority,
        };
        let rows = merge_rows(vec![
            observed(
                key.clone(),
                CoreUpdateAction::Settings(MachineTarget::Direct([7; 32])),
                1,
            ),
            observed(key, CoreUpdateAction::Local, 2),
            observed(
                MachineKey::Known {
                    build: "spaceterm-preflight".into(),
                    core: [4; 32],
                },
                CoreUpdateAction::Settings(MachineTarget::Direct([8; 32])),
                1,
            ),
        ]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].action, Some(CoreUpdateAction::Local));
        assert_eq!(
            rows[1].action,
            Some(CoreUpdateAction::Settings(MachineTarget::Direct([8; 32])))
        );
    }
}
