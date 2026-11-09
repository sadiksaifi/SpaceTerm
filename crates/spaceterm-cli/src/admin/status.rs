//! `status`: SpaceTerm Core's service facts, its Pending Version and update checks, Network
//! Access, paired devices and held outcomes.
//!
//! A running SpaceTerm Core answers over its service lane. A stopped local one is read directly,
//! without recovery, start or any other change.
use super::{Context, Opened, Pending, Reply};
use crate::contract::{
    self, Activation, Code, CoreStatus, Device, DeviceRef, Envelope, Failure, HeldOutcomes,
    MachineRef, PendingStatus, RecoveredIdentity, StatusNetwork, UnreachableData,
    UpdateCheckReason, UpdateCheckState, UpdateCheckStatus, Version,
};
use crate::state::{CliState, RouteKey};
use spaceterm_core::service::lane::{
    LaneActivation, LaneReply, LaneRequest, LaneStatus, LaneVersion, Offline,
};
use spaceterm_core::update::facts::{CheckFailure, Operation, UpdateCheck, UpdateFacts};
use std::time::{Duration, Instant};

const COMMAND: &str = "status";
const LIMIT: Duration = Duration::from_secs(30);

pub fn run(context: &Context<'_>) -> Reply {
    if !context.is_local() {
        return match context.routed() {
            Ok(routed) => over_lane(context, &routed.connection, routed.machine, routed.route),
            Err(error) => Reply::routing_failure(COMMAND, error),
        };
    }
    let core = match super::local_ref(context.identity) {
        Ok(core) => core,
        Err(failure) => return Reply::failure(COMMAND, None, failure),
    };
    // A SpaceTerm Core that answers its lane reports for itself. Otherwise the stopped service is read
    // directly, without recovery or start.
    if let Ok(connection) = context.local_connection()
        && connection
            .service_lane()
            .and_then(|lane| lane.session())
            .is_ok()
    {
        return over_lane(context, &Ok(connection), core, RouteKey::Local);
    }
    let status = Offline::new(context.identity)
        .and_then(|offline| offline.admin().status(None))
        .map_err(contract::from_core_error);
    match status {
        Ok(status) => report(context, core, &status, &RouteKey::Local, Vec::new()),
        Err(failure) => Reply::failure(COMMAND, Some(core), failure),
    }
}

fn over_lane(
    context: &Context<'_>,
    connection: &Result<spaceterm_core::MachineConnection, Failure>,
    core: MachineRef,
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
            let last = super::last_connected(context, &route);
            return Reply::new(&unreachable_envelope(core, failure, last), String::new());
        }
        Err(failure) => return Reply::failure(COMMAND, Some(core), failure),
    };
    let reply = session.request(
        &LaneRequest::Status,
        Instant::now() + context.timeout(LIMIT),
    );
    match reply {
        Ok(LaneReply::Status(status)) => report(context, core, &status, &route, recovered),
        Ok(LaneReply::Error { code }) => {
            Reply::failure(COMMAND, Some(core), contract::from_lane_code(&code))
        }
        Ok(_) => Reply::failure(COMMAND, Some(core), Failure::new(Code::ProtocolError)),
        Err(error) => Reply::failure(COMMAND, Some(core), contract::from_core_error(error)),
    }
}

/// An unreachable machine is exit 5, with when this CLI last reached it.
pub(crate) fn unreachable_envelope(
    core: MachineRef,
    failure: Failure,
    last_connected_at: Option<String>,
) -> Envelope<UnreachableData> {
    Envelope::error(
        COMMAND,
        Some(core),
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

/// Every identity of the route that holds outcomes SpaceTerm Core has not been told to forget.
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
    core: MachineRef,
    status: &LaneStatus,
    route: &RouteKey,
    recovered: Vec<crate::contract::RecoveredOperation>,
) -> Reply {
    let restart = context.command_line(&core, &["restart"]);
    let (envelope, text) = envelope(core, status, held(context, route), recovered, &restart);
    Reply::new(&envelope, text)
}

/// An RFC 3339 UTC time of Unix seconds.
pub(crate) fn rfc3339(seconds: u64) -> String {
    i64::try_from(seconds)
        .ok()
        .and_then(|seconds| jiff::Timestamp::from_second(seconds).ok())
        .map_or_else(String::new, |at| at.to_string())
}

/// The Pending Version of `facts`, as `status` and `update` report it.
pub(crate) fn pending(facts: &UpdateFacts) -> Option<PendingStatus> {
    let pending = facts.pending.as_ref()?;
    Some(PendingStatus {
        version: pending.version.to_string(),
        breaking: pending.breaking,
        installed_at: rfc3339(pending.installed_at),
        overdue_since: pending.overdue_since.map(rfc3339),
        stranded: pending
            .stranded
            .iter()
            .map(|device| DeviceRef {
                id: super::hex(&device.pairing),
                name: device.name.clone(),
            })
            .collect(),
    })
}

/// The state of the Core's feed checks. A Core with no installed runtime has never checked.
pub(crate) fn update_check(facts: Option<&UpdateFacts>) -> UpdateCheckStatus {
    let mut status = UpdateCheckStatus {
        state: UpdateCheckState::Never,
        last_success_at: None,
        failing_since: None,
        reason: None,
    };
    let Some(facts) = facts else {
        return status;
    };
    match &facts.check {
        UpdateCheck::Disabled => status.state = UpdateCheckState::Disabled,
        UpdateCheck::Never => {}
        UpdateCheck::Succeeded { at } => {
            status.state = UpdateCheckState::Succeeded;
            status.last_success_at = Some(rfc3339(*at));
        }
        UpdateCheck::Failing {
            since,
            last_success,
            reason,
        } => {
            status.state = UpdateCheckState::Failing;
            status.last_success_at = last_success.map(rfc3339);
            status.failing_since = Some(rfc3339(*since));
            status.reason = Some(match reason {
                CheckFailure::Network => UpdateCheckReason::Network,
                CheckFailure::Proxy => UpdateCheckReason::Proxy,
                CheckFailure::Verification => UpdateCheckReason::Verification,
                CheckFailure::SystemTooOld => UpdateCheckReason::SystemTooOld,
                CheckFailure::ClockBehind => UpdateCheckReason::ClockBehind,
                CheckFailure::SteppingStoneSkipped => UpdateCheckReason::SteppingStoneSkipped,
            });
        }
    }
    if facts.operation == Some(Operation::Checking) {
        status.state = UpdateCheckState::Checking;
        status.failing_since = None;
        status.reason = None;
    }
    status
}

/// The `status` envelope and its text, from the lane's status and this CLI's held outcomes.
/// `restart` is the command line that restarts this machine's SpaceTerm Core.
pub(crate) fn envelope(
    core: MachineRef,
    status: &LaneStatus,
    held_outcomes: Vec<HeldOutcomes>,
    recovered: Vec<crate::contract::RecoveredOperation>,
    restart: &str,
) -> (Envelope<CoreStatus>, String) {
    let data = CoreStatus {
        running: status.running,
        stopped: status.stopped,
        installed: status.installed,
        automatic_start: status.automatic_start,
        linger: status.linger,
        runtime: version(&status.runtime),
        pending: status.update.as_deref().and_then(pending),
        update_check: update_check(status.update.as_deref()),
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
        terminal_counters: status.terminal_counters,
    };
    let text = text(&core, &data, restart);
    (
        super::with_recovered(Envelope::ok(COMMAND, Some(core), data), recovered),
        text,
    )
}

fn text(core: &MachineRef, data: &CoreStatus, restart: &str) -> String {
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
        "SpaceTerm Core on {}: {state}\nVersion: {} (commit {})\n",
        super::display_name(core),
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
    if let Some(pending) = &data.pending {
        out.push_str(&pending_text(pending, restart));
    }
    out.push_str(&check_text(&data.update_check));
    if data.activation == Activation::Unresolved {
        out.push_str("Update: unresolved. Run spaceterm update on that machine.\n");
    }
    out.push_str(&format!("Terminal Sessions: {}\n", data.sessions));
    let network = match (&data.network.bind, data.network.enabled) {
        (Some(bind), true) if data.network.listening => format!("on, listening on {bind}"),
        (Some(bind), true) => format!("on at {bind}, not listening"),
        _ => "off".to_owned(),
    };
    out.push_str(&format!("Network Access: {network}\n"));
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

fn pending_text(pending: &PendingStatus, restart: &str) -> String {
    let overdue = if pending.overdue_since.is_some() {
        ", restart overdue"
    } else {
        ""
    };
    let mut out = format!(
        "Pending: {}, installed {}{overdue}. Restart to use it: {restart}\n",
        pending.version, pending.installed_at
    );
    if pending.breaking {
        if pending.stranded.is_empty() {
            out.push_str(
                "  It changes the protocol. Older clients stop working until they update.\n",
            );
        } else {
            let names: Vec<&str> = pending.stranded.iter().map(|v| v.name.as_str()).collect();
            out.push_str(&format!(
                "  It changes the protocol. These devices stop working until they update: {}\n",
                names.join(", ")
            ));
        }
    }
    out
}

fn check_text(check: &UpdateCheckStatus) -> String {
    let reason = |reason: Option<UpdateCheckReason>| match reason {
        Some(UpdateCheckReason::Network) => "the update feed is unreachable",
        Some(UpdateCheckReason::Proxy) => "the proxy is unreachable",
        Some(UpdateCheckReason::Verification) => "the update feed did not verify",
        Some(UpdateCheckReason::SystemTooOld) => "the newest version needs a newer system",
        Some(UpdateCheckReason::ClockBehind) => "the machine's clock is behind",
        Some(UpdateCheckReason::SteppingStoneSkipped) => {
            "the version the newest one needs first failed to start"
        }
        None => "unknown",
    };
    match check.state {
        UpdateCheckState::Disabled => "Update check: off for this build\n".to_owned(),
        UpdateCheckState::Never => "Update check: not yet run\n".to_owned(),
        UpdateCheckState::Checking => "Update check: checking now\n".to_owned(),
        UpdateCheckState::Succeeded => format!(
            "Update check: succeeded {}\n",
            check.last_success_at.as_deref().unwrap_or_default()
        ),
        UpdateCheckState::Failing => format!(
            "Update check: failing since {}, {}\n",
            check.failing_since.as_deref().unwrap_or_default(),
            reason(check.reason)
        ),
    }
}
