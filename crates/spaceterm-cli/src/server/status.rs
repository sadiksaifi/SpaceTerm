//! `server status`: service facts, Network Access, paired devices and held outcomes.
//!
//! A running server answers over its service lane. A stopped local server is read directly,
//! without recovery, start or any other change.
use super::{Context, Opened, Pending, Reply};
use crate::contract::{
    self, Activation, Code, Device, Envelope, Failure, HeldOutcomes, RecoveredIdentity, ServerRef,
    ServerStatus, StagedVersion, StatusNetwork, UnreachableData, Version,
};
use crate::state::{CliState, RouteKey};
use spaceterm_server::service::lane::{
    LaneActivation, LaneReply, LaneRequest, LaneStatus, LaneVersion, Offline,
};
use std::time::{Duration, Instant};

const COMMAND: &str = "server.status";
const LIMIT: Duration = Duration::from_secs(30);

pub fn run(context: &Context<'_>) -> Reply {
    if let super::Target::Remote(_) = context.target() {
        return match context.routed() {
            Ok(routed) => over_lane(context, &routed.connection, routed.server, routed.route),
            Err(error) => Reply::routing_failure(COMMAND, error),
        };
    }
    let server = match super::local_ref(context.identity) {
        Ok(server) => server,
        Err(failure) => return Reply::failure(COMMAND, None, failure),
    };
    // A server that answers its lane reports for itself. Otherwise the stopped service is read
    // directly, without recovery or start.
    if let Ok(connection) = context.local_connection()
        && connection
            .service_lane()
            .and_then(|lane| lane.session())
            .is_ok()
    {
        return over_lane(context, &Ok(connection), server, RouteKey::Local);
    }
    let status = Offline::new(context.identity)
        .and_then(|offline| offline.admin().status(None))
        .map_err(contract::from_server_error);
    match status {
        Ok(status) => report(context, server, &status, &RouteKey::Local, Vec::new()),
        Err(failure) => Reply::failure(COMMAND, Some(server), failure),
    }
}

fn over_lane(
    context: &Context<'_>,
    connection: &Result<spaceterm_server::ServerConnection, Failure>,
    server: ServerRef,
    route: RouteKey,
) -> Reply {
    let opened = Pending::open(context, &route)
        .and_then(|pending| super::open_lane(context, connection, &route, &pending));
    let Opened {
        mut session,
        recovered,
        ..
    } = match opened {
        Ok(opened) => opened,
        Err(failure) if failure.code == Code::Unreachable || failure.code == Code::NotRunning => {
            return unreachable(server, failure);
        }
        Err(failure) => return Reply::failure(COMMAND, Some(server), failure),
    };
    let reply = session.request(
        &LaneRequest::Status,
        Instant::now() + context.timeout(LIMIT),
    );
    match reply {
        Ok(LaneReply::Status(status)) => report(context, server, &status, &route, recovered),
        Ok(LaneReply::Error { code }) => {
            Reply::failure(COMMAND, Some(server), contract::from_lane_code(&code))
        }
        Ok(_) => Reply::failure(COMMAND, Some(server), Failure::new(Code::ProtocolError)),
        Err(error) => Reply::failure(COMMAND, Some(server), contract::from_server_error(error)),
    }
}

/// An unreachable server is exit 5, with when this CLI last reached it.
fn unreachable(server: ServerRef, failure: Failure) -> Reply {
    Reply::new(&unreachable_envelope(server, failure, None), String::new())
}

pub(crate) fn unreachable_envelope(
    server: ServerRef,
    failure: Failure,
    last_connected_at: Option<String>,
) -> Envelope<UnreachableData> {
    Envelope::error(
        COMMAND,
        Some(server),
        failure,
        UnreachableData { last_connected_at },
    )
}

pub fn version(value: &LaneVersion) -> Version {
    Version {
        version: value.version.clone(),
        commit: value.commit.clone(),
        sha256: value.sha256.clone(),
    }
}

/// Every identity of the route that holds outcomes the server has not been told to forget.
fn held(context: &Context<'_>, route: &RouteKey) -> Vec<HeldOutcomes> {
    let mut held: Vec<HeldOutcomes> = CliState::open(context.identity)
        .and_then(|state| state.held(route))
        .unwrap_or_default()
        .into_iter()
        .map(|identity| HeldOutcomes {
            identity: identity.identity,
            count: identity.operations + identity.runs,
        })
        .collect();
    // This identity's own unacknowledged service operations.
    if let Ok(pending) = Pending::open(context, route).and_then(|pending| pending.list())
        && !pending.is_empty()
    {
        let identity = context.identity_kind();
        let count = u32::try_from(pending.len()).unwrap_or(u32::MAX);
        match held.iter_mut().find(|v| v.identity == identity) {
            Some(entry) => entry.count += count,
            None => held.push(HeldOutcomes { identity, count }),
        }
    }
    held.retain(|v| v.count > 0);
    held
}

fn report(
    context: &Context<'_>,
    server: ServerRef,
    status: &LaneStatus,
    route: &RouteKey,
    recovered: Vec<crate::contract::RecoveredOperation>,
) -> Reply {
    let (envelope, text) = envelope(server, status, held(context, route), recovered);
    Reply::new(&envelope, text)
}

/// The `server.status` envelope and its text, from the lane's status and this CLI's held
/// outcomes.
pub(crate) fn envelope(
    server: ServerRef,
    status: &LaneStatus,
    held_outcomes: Vec<HeldOutcomes>,
    recovered: Vec<crate::contract::RecoveredOperation>,
) -> (Envelope<ServerStatus>, String) {
    let data = ServerStatus {
        running: status.running,
        stopped: status.stopped,
        installed: status.installed,
        automatic_start: status.automatic_start,
        linger: status.linger,
        runtime: version(&status.runtime),
        staged: status
            .staged
            .iter()
            .map(|v| StagedVersion {
                version: v.version.clone(),
                sha256: v.sha256.clone(),
            })
            .collect(),
        activation: match status.activation {
            LaneActivation::Settled => Activation::Settled,
            LaneActivation::Unresolved => Activation::Unresolved,
        },
        sessions: status.sessions,
        network: StatusNetwork {
            enabled: status.network.enabled,
            bind: status.network.bind.clone(),
            listening: status.network.listening,
            addresses: status.network.addresses.clone(),
        },
        devices: status
            .devices
            .iter()
            .map(|v| Device {
                id: v.id.clone(),
                name: v.name.clone(),
                fingerprint: v.fingerprint.clone(),
                last_connected_at: v.last_connected_at.clone(),
            })
            .collect(),
        held_outcomes,
        lane_major: status.lane_major,
    };
    let text = text(&server, &data);
    (
        super::with_recovered(Envelope::ok(COMMAND, Some(server), data), recovered),
        text,
    )
}

fn text(server: &ServerRef, data: &ServerStatus) -> String {
    let state = if !data.installed {
        "not installed"
    } else if data.running {
        "running"
    } else if data.stopped {
        "stopped"
    } else {
        "not running"
    };
    let mut out = format!(
        "Server {}: {state}\nVersion: {} (commit {})\n",
        server.name,
        data.runtime.version,
        data.runtime.commit.get(..7).unwrap_or(&data.runtime.commit)
    );
    out.push_str(&format!(
        "Automatic start: {}\n",
        if data.automatic_start { "on" } else { "off" }
    ));
    if let Some(linger) = data.linger {
        out.push_str(&format!(
            "Boot startup: {}\n",
            if linger {
                "authorized"
            } else {
                "not authorized"
            }
        ));
    }
    if data.activation == Activation::Unresolved {
        out.push_str("Update: unresolved. Run spaceterm server update on this machine.\n");
    }
    out.push_str(&format!("Terminal Sessions: {}\n", data.sessions));
    let network = match (&data.network.bind, data.network.enabled) {
        (Some(bind), true) if data.network.listening => format!("on, listening on {bind}"),
        (Some(bind), true) => format!("on at {bind}, not listening"),
        _ => "off".to_owned(),
    };
    out.push_str(&format!("Network Access: {network}\n"));
    for staged in &data.staged {
        out.push_str(&format!("Staged: {}\n", staged.version));
    }
    if data.devices.is_empty() {
        out.push_str("Paired devices: none\n");
    } else {
        out.push_str("Paired devices:\n");
        for device in &data.devices {
            out.push_str(&format!(
                "  {}  id:{}  {}\n",
                device.name,
                device.id.get(..8).unwrap_or(&device.id),
                device.fingerprint
            ));
        }
    }
    for held in &data.held_outcomes {
        let identity = match &held.identity {
            RecoveredIdentity::Base => "this CLI".to_owned(),
            RecoveredIdentity::Agent(agent) => format!("agent {}", agent.as_str()),
        };
        out.push_str(&format!("Held outcomes: {} for {identity}\n", held.count));
    }
    out
}
