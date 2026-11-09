//! `status`: SpaceTerm Core's service facts, its Pending Version and update checks, Network
//! Access, paired devices and held outcomes.
//!
//! A running SpaceTerm Core answers over its service lane. A stopped local one is read directly,
//! without recovery, start or any other change.
use super::{Context, Opened, Pending, Reply};
use crate::contract::{
    self, Activation, Code, CoreStatus, Device, DeviceRef, Envelope, Failure, HeldOutcomes,
    MachineRef, PendingStatus, RecoveredIdentity, StatusNetwork, UnreachableData,
    UpdateCheckReason, UpdateCheckState, UpdateCheckStatus, UpdateFailureReason,
    UpdateFailureStage, UpdateFailureStatus, UpdateOperation, UpdateOperationKind, Version,
};
use crate::state::{CliState, RouteKey};
use spaceterm_core::service::lane::{
    LaneActivation, LaneReply, LaneRequest, LaneStatus, LaneVersion, Offline,
};
use spaceterm_core::update::facts::{
    CheckFailure, FailureKind, InstallFailure, OVERDUE_AFTER, Operation, StartFailure, UpdateCheck,
    UpdateFacts,
};
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
    let commands = Commands {
        restart: &restart,
        local: *route == RouteKey::Local,
    };
    let (envelope, text) = envelope(
        core,
        status,
        held(context, route),
        recovered,
        &commands,
        crate::notice::now(),
    );
    Reply::new(&envelope, text)
}

/// An RFC 3339 UTC time of Unix seconds.
pub(crate) fn rfc3339(seconds: u64) -> String {
    i64::try_from(seconds)
        .ok()
        .and_then(|seconds| jiff::Timestamp::from_second(seconds).ok())
        .map_or_else(String::new, |at| at.to_string())
}

/// The Pending Version of `facts` at `now`, Unix seconds, as `status` and `update` report it.
/// It is overdue by the two-day policy, whichever Core reported it: one of 0.11.2 or earlier
/// marks `overdue_since` after one day.
pub(crate) fn pending(facts: &UpdateFacts, now: u64) -> Option<PendingStatus> {
    let pending = facts.pending.as_ref()?;
    Some(PendingStatus {
        version: pending.version.to_string(),
        breaking: pending.breaking,
        installed_at: rfc3339(pending.installed_at),
        overdue_since: pending
            .overdue(now)
            .then(|| rfc3339(pending.installed_at.saturating_add(OVERDUE_AFTER))),
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

/// The update operation of `facts`, as `status` reports it.
pub(crate) fn operation(facts: &UpdateFacts) -> Option<UpdateOperation> {
    let plain = |kind| UpdateOperation {
        kind,
        version: None,
        received: None,
        total: None,
    };
    Some(match facts.operation.as_ref()? {
        Operation::Checking => plain(UpdateOperationKind::Checking),
        Operation::Installing {
            version,
            received,
            total,
        } => UpdateOperation {
            kind: UpdateOperationKind::Installing,
            version: Some(version.to_string()),
            received: Some(*received),
            total: Some(*total),
        },
        Operation::Restarting { to } => UpdateOperation {
            version: Some(to.to_string()),
            ..plain(UpdateOperationKind::Restarting)
        },
        Operation::RestartingSameVersion => plain(UpdateOperationKind::RestartingSameVersion),
        Operation::Stopping => plain(UpdateOperationKind::Stopping),
        Operation::Uninstalling => plain(UpdateOperationKind::Uninstalling),
    })
}

/// The stage and reason of an update failure.
pub(crate) fn failure_reason(kind: FailureKind) -> (UpdateFailureStage, UpdateFailureReason) {
    use UpdateFailureReason as R;
    match kind {
        FailureKind::Install(failure) => (
            UpdateFailureStage::Install,
            match failure {
                InstallFailure::DiskFull => R::DiskFull,
                InstallFailure::Download => R::Download,
                InstallFailure::Verification => R::Verification,
            },
        ),
        FailureKind::Start(failure) => (
            UpdateFailureStage::Start,
            match failure {
                StartFailure::NotReady => R::NotReady,
                StartFailure::PortInUse => R::PortInUse,
                StartFailure::DiskFull => R::DiskFull,
                StartFailure::Interrupted => R::Interrupted,
            },
        ),
    }
}

/// The last update failure of `facts`, as `status` reports it.
pub(crate) fn last_failure(facts: &UpdateFacts) -> Option<UpdateFailureStatus> {
    let failure = facts.last_failure.as_ref()?;
    let (stage, reason) = failure_reason(failure.kind);
    Some(UpdateFailureStatus {
        at: rfc3339(failure.at),
        version: failure.version.to_string(),
        stage,
        reason,
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

/// What the `status` text tells the person to run.
pub(crate) struct Commands<'a> {
    /// The command line that restarts this machine's SpaceTerm Core.
    pub(crate) restart: &'a str,
    /// The machine is this one, so `spaceterm start` runs here.
    pub(crate) local: bool,
}

/// The `status` envelope and its text, from the lane's status and this CLI's held outcomes.
pub(crate) fn envelope(
    core: MachineRef,
    status: &LaneStatus,
    held_outcomes: Vec<HeldOutcomes>,
    recovered: Vec<crate::contract::RecoveredOperation>,
    commands: &Commands<'_>,
    now: u64,
) -> (Envelope<CoreStatus>, String) {
    let facts = status.update.as_deref();
    let data = CoreStatus {
        running: status.running,
        stopped: status.stopped,
        installed: status.installed,
        automatic_start: status.automatic_start,
        linger: status.linger,
        runtime: version(&status.runtime),
        pending: facts.and_then(|facts| pending(facts, now)),
        update_check: update_check(facts),
        operation: facts.and_then(operation),
        skipped: facts.and_then(|facts| facts.skipped.as_ref().map(ToString::to_string)),
        last_failure: facts.and_then(last_failure),
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
    let text = text(&core, &data, commands);
    (
        super::with_recovered(Envelope::ok(COMMAND, Some(core), data), recovered),
        text,
    )
}

fn text(core: &MachineRef, data: &CoreStatus, commands: &Commands<'_>) -> String {
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
        out.push_str(&pending_text(pending, commands.restart));
    }
    out.push_str(&check_text(&data.update_check));
    let restarting = data.operation.as_ref().is_some_and(|operation| {
        matches!(
            operation.kind,
            UpdateOperationKind::Restarting | UpdateOperationKind::RestartingSameVersion
        )
    });
    if let Some(operation) = &data.operation {
        out.push_str(&operation_text(operation, &data.runtime.version));
    }
    if data.activation == Activation::Unresolved && !restarting {
        let finish = if commands.local {
            "finish it with: spaceterm start"
        } else {
            "finish it on that machine with: spaceterm start"
        };
        out.push_str(&format!(
            "Update: the restart into the new version has not finished. Check again in a minute. If it still has not finished, {finish}\n"
        ));
    }
    if let Some(skipped) = &data.skipped {
        out.push_str(&format!(
            "Skipped: {skipped} failed to start, so SpaceTerm Core refuses it until a newer version appears.\n"
        ));
    }
    if let Some(failure) = &data.last_failure {
        out.push_str(&failure_text(failure));
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

/// The line of an update operation other than a feed check, which the update check line names.
fn operation_text(operation: &UpdateOperation, running: &str) -> String {
    let version = operation.version.as_deref().unwrap_or_default();
    match operation.kind {
        UpdateOperationKind::Checking => String::new(),
        UpdateOperationKind::Installing => {
            let (received, total) = (
                operation.received.unwrap_or_default(),
                operation.total.unwrap_or_default(),
            );
            if total > 0 && received < total {
                format!(
                    "In progress: installing {version}, {}% received\n",
                    received.saturating_mul(100) / total
                )
            } else {
                format!("In progress: installing {version}, verifying and unpacking\n")
            }
        }
        UpdateOperationKind::Restarting => {
            format!("In progress: restarting from {running} to {version}\n")
        }
        UpdateOperationKind::RestartingSameVersion => {
            format!("In progress: restarting {running} without updating\n")
        }
        UpdateOperationKind::Stopping => "In progress: stopping\n".to_owned(),
        UpdateOperationKind::Uninstalling => "In progress: removing SpaceTerm Core\n".to_owned(),
    }
}

fn failure_text(failure: &UpdateFailureStatus) -> String {
    let version = &failure.version;
    let what = match (failure.stage, failure.reason) {
        (UpdateFailureStage::Install, UpdateFailureReason::DiskFull) => {
            format!("installing {version} failed because the disk is full. Nothing changed")
        }
        (UpdateFailureStage::Install, UpdateFailureReason::Download) => {
            format!("downloading {version} failed. Nothing changed")
        }
        (UpdateFailureStage::Install, _) => {
            format!("{version} did not verify. Nothing changed")
        }
        (UpdateFailureStage::Start, UpdateFailureReason::NotReady) => format!(
            "{version} did not become ready, so SpaceTerm Core returned to the version it ran before"
        ),
        (UpdateFailureStage::Start, UpdateFailureReason::PortInUse) => format!(
            "{version} couldn\u{2019}t listen on the Network Access port because another program holds it, so SpaceTerm Core returned to the version it ran before. It tries {version} again at the next restart"
        ),
        (UpdateFailureStage::Start, UpdateFailureReason::DiskFull) => format!(
            "{version} couldn\u{2019}t start because the disk is full, so SpaceTerm Core returned to the version it ran before. It tries {version} again at the next restart"
        ),
        (UpdateFailureStage::Start, _) => format!(
            "a reboot, logout or crash interrupted the restart into {version}. It tries {version} again at the next restart"
        ),
    };
    format!("Last update failure: {}, {what}.\n", failure.at)
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
