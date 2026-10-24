//! Passive server inventory for the Updates dropdown. Routes identify servers; they do not
//! create maintenance authority. Only the existing Settings journeys prepare or activate updates.

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{App, Context, SharedString, Subscription, Task};

use crate::application_identity::ApplicationIdentity;
use crate::direct_connections::{DirectConnections, DirectRouteStatus};
use crate::domain::SshDestination;
use crate::local_server::ServerError;
use crate::local_server::service::Service;
use crate::local_server::service::activation::is_released_downgrade;
use crate::local_server::service::maintenance::{MaintenanceFacts, RemoteActivationPhase};
use crate::ssh::server_connection::{
    RemoteConnectionStatus, ServerCandidate, SshServerConnections, SshServerContinuity,
};
use crate::ssh::server_operations::{RemoteOperationState, RemoteServerOperations};
use crate::ui::MachineTarget;
use crate::ui::direct_servers::DirectChanges;
use crate::ui::remote_maintenance::RemoteAction;
use crate::ui::settings_window;

/// Each status is a few words that fit the menu. The action's Settings journey explains it.
const UPDATE_STATUS_UNKNOWN: &str = "Update Status Unknown";
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
    Ssh(SshDestination),
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
            Self::Local | Self::Ssh(_) => "Update Server…",
            Self::Review(_) | Self::Settings(_) => "Settings…",
        }
    }

    pub(crate) fn need(&self) -> Option<ServerNeed> {
        match self {
            Self::Local | Self::Ssh(_) => Some(ServerNeed::Update),
            Self::Review(_) => Some(ServerNeed::Attention),
            Self::Settings(_) => None,
        }
    }

    pub(crate) fn activate(&self, cx: &mut App) {
        match self {
            Self::Local => settings_window::open_server_update(cx),
            Self::Ssh(destination) => settings_window::open_remote_maintenance(
                destination.clone(),
                RemoteAction::Update,
                cx,
            ),
            Self::Review(target) | Self::Settings(target) => {
                settings_window::open_server_settings(target.clone(), cx)
            }
        }
    }
}

#[derive(Clone)]
struct Sources {
    ssh: Option<SshServerConnections>,
    direct: Option<DirectConnections>,
    operations: Option<Arc<dyn RemoteServerOperations>>,
}

/// Owns background reads independently of the Settings Window. Rendering clones its snapshot.
pub(crate) struct ServerUpdates {
    sources: Sources,
    rows: Vec<ServerUpdateRow>,
    reading: bool,
    pending: bool,
    _read: Option<Task<()>>,
    _followers: Vec<Task<()>>,
    _direct: Option<DirectChanges>,
    _local: Subscription,
}

impl ServerUpdates {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let local = settings_window::local_operations::LocalServerOperations::get(cx);
        let local = cx.subscribe(&local, |inventory, _, _, cx| inventory.refresh(cx));
        let sources = Sources {
            ssh: crate::ui::ssh_servers::installed(cx),
            direct: crate::ui::direct_servers::installed(cx),
            operations: crate::ssh::server_operations::installed(cx),
        };
        let mut followers = Vec::new();
        for changes in sources
            .ssh
            .as_ref()
            .map(SshServerConnections::subscribe)
            .into_iter()
            .chain(sources.operations.as_ref().map(|owner| owner.subscribe()))
        {
            followers.push(cx.spawn(async move |entity, cx| {
                while changes.recv().await.is_ok() {
                    if entity
                        .update(cx, |inventory, cx| inventory.refresh(cx))
                        .is_err()
                    {
                        break;
                    }
                }
            }));
        }
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
                label: "This computer · this account".into(),
                detail: "Reading Update Status…".into(),
                action: Some(ServerUpdateAction::Settings(MachineTarget::Local)),
            }],
            reading: false,
            pending: false,
            _read: None,
            _followers: followers,
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
    UnknownSsh(SshDestination),
}

struct Observation {
    key: ServerKey,
    row: ServerUpdateRow,
    /// Prefer an available update's authorized route, then local authority, then SSH, then WSS.
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
        route_priority: 3,
    };
    let mut observations = vec![local];
    let entries = sources
        .operations
        .as_ref()
        .map(|owner| owner.entries())
        .unwrap_or_default();
    if let Some(ssh) = sources.ssh {
        // Several aliases often reach one native target. Verify its archive once per snapshot.
        let mut candidates = HashMap::new();
        for (destination, status) in ssh.connections() {
            let continuity = ssh.continuity(&destination);
            let key = continuity.as_ref().map_or_else(
                || ServerKey::UnknownSsh(destination.clone()),
                |identity| ServerKey::Known {
                    build: identity.build.clone(),
                    server: identity.server,
                },
            );
            let candidate = ssh.last_facts(&destination).and_then(|facts| {
                candidates
                    .entry(facts.platform.clone())
                    .or_insert_with(|| ssh.verified_candidate(&facts.platform))
                    .clone()
            });
            let state = entries
                .iter()
                .find(|entry| !entry.removed && entry.destination == destination)
                .map(|entry| &entry.state);
            observations.push(Observation {
                key,
                row: ssh_row(
                    destination,
                    &status,
                    continuity.as_ref(),
                    state,
                    candidate.as_ref(),
                ),
                route_priority: 2,
            });
        }
    }
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
                    label: format!("{} · account unknown", route.label).into(),
                    // A paired network connection grants no server maintenance authority.
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
        label: "This computer · this account".into(),
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

fn ssh_row(
    destination: SshDestination,
    status: &RemoteConnectionStatus,
    continuity: Option<&SshServerContinuity>,
    state: Option<&RemoteOperationState>,
    candidate: Option<&ServerCandidate>,
) -> ServerUpdateRow {
    let label = continuity.map_or_else(
        || format!("{} · account unknown", destination.as_str()),
        |identity| format!("{} · account {}", destination.as_str(), identity.account),
    );
    let known = matches!(
        status,
        RemoteConnectionStatus::Connected { .. }
            | RemoteConnectionStatus::Checked {
                availability: crate::ssh::server_connection::ServerAvailability::Running
                    | crate::ssh::server_connection::ServerAvailability::NotRunning,
                ..
            }
            | RemoteConnectionStatus::Stopped
    );
    let facts = match (continuity, state) {
        (Some(identity), Some(RemoteOperationState::Read(facts)))
            if identity.server == facts.server
                && identity.account == facts.account
                && identity.build == facts.build =>
        {
            Some(facts.as_ref())
        }
        _ => None,
    };
    let (detail, need) = match (known, facts) {
        (false, _) => ("Offline or Status Unknown", None),
        (true, Some(facts)) => remote_update(facts, candidate),
        (true, None) => (UPDATE_STATUS_UNKNOWN, None),
    };
    let target = MachineTarget::Ssh(destination.clone());
    ServerUpdateRow {
        label: label.into(),
        detail: detail.into(),
        action: Some(match need {
            Some(ServerNeed::Update) => ServerUpdateAction::Ssh(destination),
            Some(ServerNeed::Attention) => ServerUpdateAction::Review(target),
            None => ServerUpdateAction::Settings(target),
        }),
    }
}

fn remote_update(
    facts: &MaintenanceFacts,
    candidate: Option<&ServerCandidate>,
) -> (&'static str, Option<ServerNeed>) {
    if !facts.status.installed {
        return ("Not Installed", None);
    }
    if facts.activation.as_ref().is_some_and(|activation| {
        !activation.returned && !matches!(activation.phase, RemoteActivationPhase::Completed { .. })
    }) {
        return (UPDATE_NEEDS_ATTENTION, Some(ServerNeed::Attention));
    }
    let Some(approved) = &facts.approved else {
        return (UPDATE_STATUS_UNKNOWN, None);
    };
    let Some(candidate) = candidate else {
        return ("No Update", None);
    };
    if candidate.digest == approved.digest {
        return ("Up to Date", None);
    }
    if is_released_downgrade(&candidate.version, &approved.version) {
        return ("No Update", None);
    }
    ("Update Available", Some(ServerNeed::Update))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_server::service::maintenance::{RemoteActivation, RemoteBuild};
    use crate::local_server::service::{ServiceStartup, ServiceStatus};
    use crate::ssh::server_connection::{SshServerFacts, UnavailableKind};

    fn destination(name: &str) -> SshDestination {
        SshDestination::new(name.to_owned()).unwrap()
    }

    fn continuity() -> SshServerContinuity {
        SshServerContinuity {
            route: [9; 32],
            account: 501,
            build: "spaceterm-preflight".into(),
            server: [3; 32],
        }
    }

    fn facts() -> MaintenanceFacts {
        MaintenanceFacts {
            server: [3; 32],
            account: 501,
            build: "spaceterm-preflight".into(),
            status: ServiceStatus {
                installed: true,
                enabled: true,
                running: true,
                stopped: false,
            },
            startup: ServiceStartup::AtLogin,
            approved: Some(RemoteBuild {
                version: "1.4.0".into(),
                commit: "a".repeat(40),
                digest: "a".repeat(64),
            }),
            activation: None,
        }
    }

    fn connected() -> RemoteConnectionStatus {
        RemoteConnectionStatus::Connected {
            protocol_major: 1,
            facts: SshServerFacts {
                platform: ("macOS".into(), "arm64".into()),
                version: Some("1.4.0".into()),
                starts_automatically: true,
            },
        }
    }

    fn row(facts: MaintenanceFacts, candidate: Option<&ServerCandidate>) -> ServerUpdateRow {
        ssh_row(
            destination("macbookpro"),
            &connected(),
            Some(&continuity()),
            Some(&RemoteOperationState::Read(Box::new(facts))),
            candidate,
        )
    }

    #[test]
    fn a_release_number_never_substitutes_for_verified_whole_artifact_evidence() {
        let candidate = ServerCandidate {
            version: "1.5.0".into(),
            digest: "b".repeat(64),
        };
        assert_eq!(
            row(facts(), None).action,
            Some(ServerUpdateAction::Settings(MachineTarget::Ssh(
                destination("macbookpro")
            )))
        );
        let mut approved = facts();
        approved.approved.as_mut().unwrap().digest = candidate.digest.clone();
        assert!(
            !row(approved, Some(&candidate))
                .action
                .unwrap()
                .need()
                .is_some()
        );
        let mut same_version = candidate.clone();
        same_version.version = "1.4.0".into();
        assert_eq!(
            row(facts(), Some(&same_version)).action,
            Some(ServerUpdateAction::Ssh(destination("macbookpro")))
        );
        let mut downgrade = candidate.clone();
        downgrade.version = "1.3.9".into();
        assert!(
            !row(facts(), Some(&downgrade))
                .action
                .unwrap()
                .need()
                .is_some()
        );
        let mut unknown = facts();
        unknown.approved = None;
        assert!(
            !row(unknown, Some(&candidate))
                .action
                .unwrap()
                .need()
                .is_some()
        );
    }

    #[test]
    fn cached_candidate_and_service_facts_do_not_offer_an_offline_or_changed_server_update() {
        let candidate = ServerCandidate {
            version: "1.5.0".into(),
            digest: "b".repeat(64),
        };
        let state = RemoteOperationState::Read(Box::new(facts()));
        let offline = ssh_row(
            destination("macbookpro"),
            &RemoteConnectionStatus::Unavailable {
                kind: UnavailableKind::Network,
            },
            Some(&continuity()),
            Some(&state),
            Some(&candidate),
        );
        assert!(offline.detail.contains("Offline"));
        assert_eq!(offline.action.unwrap().need(), None);
        let mut changed = continuity();
        changed.account += 1;
        let row = ssh_row(
            destination("macbookpro"),
            &connected(),
            Some(&changed),
            Some(&state),
            Some(&candidate),
        );
        assert_eq!(&*row.detail, UPDATE_STATUS_UNKNOWN);
        assert_eq!(row.action.unwrap().need(), None);
    }

    #[test]
    fn a_stopped_update_needs_attention_while_offline_unknown_and_unauthorized_status_asks_nothing()
    {
        let mut interrupted = facts();
        interrupted.activation = Some(RemoteActivation {
            operation: [5; 32],
            previous: facts().approved.unwrap(),
            candidate: RemoteBuild {
                version: "1.5.0".into(),
                commit: "b".repeat(40),
                digest: "b".repeat(64),
            },
            stopped: false,
            phase: RemoteActivationPhase::Uncertain,
            returned: false,
        });
        let attention = row(interrupted, None);
        assert_eq!(&*attention.detail, "Update Needs Attention");
        assert_eq!(
            attention.action,
            Some(ServerUpdateAction::Review(MachineTarget::Ssh(destination(
                "macbookpro"
            ))))
        );
        assert_eq!(
            attention.action.unwrap().need(),
            Some(ServerNeed::Attention)
        );

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
        let offline = ssh_row(
            destination("macbookpro"),
            &RemoteConnectionStatus::Unavailable {
                kind: UnavailableKind::Network,
            },
            None,
            None,
            None,
        );
        let unread = ssh_row(
            destination("macbookpro"),
            &connected(),
            Some(&continuity()),
            None,
            None,
        );
        for quiet in [unknown, offline, unread] {
            assert_eq!(quiet.action.unwrap().need(), None, "{}", quiet.detail);
        }
    }

    #[test]
    fn local_ssh_aliases_and_wss_routes_merge_by_server_identity_and_select_account_authority() {
        let candidate = ServerCandidate {
            version: "1.5.0".into(),
            digest: "b".repeat(64),
        };
        let key = ServerKey::Known {
            build: "spaceterm-preflight".into(),
            server: [3; 32],
        };
        let observed = |key, action, priority| Observation {
            key,
            row: ServerUpdateRow {
                label: "machine · account 501".into(),
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
            observed(
                key.clone(),
                ServerUpdateAction::Settings(MachineTarget::Local),
                3,
            ),
            Observation {
                key: key.clone(),
                row: row(facts(), Some(&candidate)),
                route_priority: 2,
            },
            observed(key, ServerUpdateAction::Ssh(destination("other-alias")), 2),
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
        assert_eq!(
            rows[0].action,
            Some(ServerUpdateAction::Ssh(destination("macbookpro")))
        );
        assert_eq!(
            rows[1].action,
            Some(ServerUpdateAction::Settings(MachineTarget::Direct([8; 32])))
        );
        let rows = merge_rows(vec![
            observed(
                ServerKey::UnknownSsh(destination("first")),
                ServerUpdateAction::Settings(MachineTarget::Ssh(destination("first"))),
                2,
            ),
            observed(
                ServerKey::UnknownSsh(destination("second")),
                ServerUpdateAction::Settings(MachineTarget::Ssh(destination("second"))),
                2,
            ),
        ]);
        assert_eq!(
            rows.len(),
            2,
            "unobserved aliases cannot be assumed to share a server"
        );
    }
}
