//! `server network`: change Network Access through the service lane, on the server's own machine
//! or from a Paired Device.
//!
//! The preview reports Network Access before and after the change, whether every paired
//! connection ends, what becomes of this device's own connection, and which connected devices
//! lose their route. The server refuses a paired caller's change that leaves no connected device a
//! route, as `disconnects_every_device`. A confirmed change follows the service lane's operation
//! flow: when the change ends this connection, the flow reconnects and resolves the same
//! operation. A device that lost its route cannot reconnect, so it reports `outcome_unconfirmed`
//! once the wait ends.
use super::lifecycle::Settled;
use super::{Context, Opened, Pending, Reply, Routed};
use crate::args::Network;
use crate::contract::{
    self, CallerEffect, Code, Confirmation, ConfirmationData, DeviceRef, Envelope, Failure,
    NetworkEffects, NetworkSetting, RecoveredOperation, ServerNetwork, ServerRef, ServiceAction,
};
use spaceterm_server::service::lane::admin;
use spaceterm_server::service::lane::client::{Lane, within};
use spaceterm_server::service::lane::network::{
    LaneCallerEffect, LaneNetworkEffects, LaneNetworkSetting, NetworkAccess,
};
use spaceterm_server::service::lane::{LaneAction, LaneReply, LaneRequest};
use std::time::{Duration, Instant};

const COMMAND: &str = "server.network";
/// How long the command reads Network Access back after a completed change.
const READ_BACK: Duration = Duration::from_secs(10);

pub fn run(context: &Context<'_>, network: &Network, token: Option<&str>) -> Reply {
    let access = access(network);
    let routed = match context.routed() {
        Ok(routed) => routed,
        Err(error) => return Reply::routing_failure(COMMAND, error),
    };
    let pending = match Pending::open(context, &routed.route) {
        Ok(pending) => pending,
        Err(failure) => return Reply::failure(COMMAND, Some(routed.server), failure),
    };
    let opened = match super::open_lane(context, &routed.connection, &routed.route, &pending) {
        Ok(opened) => opened,
        Err(failure) => {
            let failure = match failure.code {
                Code::Unreachable if routed.local => Failure::new(Code::NotRunning),
                _ => failure,
            };
            return Reply::failure(COMMAND, Some(routed.server), failure);
        }
    };
    match token {
        Some(token) => execute(context, access, routed, opened, &pending, token.to_owned()),
        None => preview(context, access, routed, opened, &pending),
    }
}

fn access(network: &Network) -> NetworkAccess {
    match network {
        Network::Off => NetworkAccess::Off,
        Network::All => NetworkAccess::All,
        Network::Address(policy) => NetworkAccess::Address(policy.bind),
    }
}

/// The command-line form of `access`, for the confirmation hint.
fn argument(access: NetworkAccess) -> String {
    match access {
        NetworkAccess::Off => "off".to_owned(),
        NetworkAccess::All => "all".to_owned(),
        NetworkAccess::Address(address) => address.to_string(),
    }
}

fn preview(
    context: &Context<'_>,
    access: NetworkAccess,
    routed: Routed,
    mut opened: Opened,
    pending: &Pending,
) -> Reply {
    let admin::NetworkPreview {
        token,
        expires_at,
        effects,
        ..
    } = match admin::preview_network(&mut opened.session, access) {
        Ok(preview) => preview,
        Err(error) => return failure(routed.server, super::lane_failure(error), opened),
    };
    if effects.unchanged() {
        let data = ServerNetwork {
            network: Some(setting(&effects.to)),
            operation: None,
            receipt: None,
        };
        let text = format!("Network Access is already {}.\n", describe(&effects.to));
        return Reply::new(
            &super::with_recovered(
                Envelope::ok(COMMAND, Some(routed.server), data),
                opened.recovered,
            ),
            text,
        );
    }
    let summary = summary(&routed.server.name, &effects);
    if context.interactive() {
        let warnings: Vec<_> = super::recovered_warning(&opened.recovered)
            .into_iter()
            .collect();
        contract::write_warnings(&warnings, &mut std::io::stderr().lock());
        eprint!("{summary}");
        if super::confirm("Change Network Access?") {
            return execute(context, access, routed, opened, pending, token);
        }
        return Reply::failure(
            COMMAND,
            Some(routed.server),
            Failure::new(Code::ConfirmationRequired),
        );
    }
    let text = format!(
        "{summary}Confirm with: {}\n",
        confirm_line(context, &routed.server, access, &token)
    );
    Reply::new(
        &preview_envelope(routed.server, token, expires_at, &effects, opened.recovered),
        text,
    )
}

/// The confirmation a person copies: the same change, target and actor.
pub(crate) fn confirm_line(
    context: &Context<'_>,
    server: &ServerRef,
    access: NetworkAccess,
    token: &str,
) -> String {
    context.command_line(
        server,
        &format!("server network {} --confirm {token}", argument(access)),
    )
}

fn failure(server: ServerRef, failure: Failure, opened: Opened) -> Reply {
    Reply::new(
        &super::with_recovered(
            Envelope::error(COMMAND, Some(server), failure, contract::Empty {}),
            opened.recovered,
        ),
        String::new(),
    )
}

/// The confirmation envelope of a Network Access preview.
pub(crate) fn preview_envelope(
    server: ServerRef,
    token: String,
    expires_at: String,
    effects: &LaneNetworkEffects,
    recovered: Vec<RecoveredOperation>,
) -> Envelope<ConfirmationData<NetworkEffects>> {
    super::with_recovered(
        Envelope::confirmation(
            COMMAND,
            Some(server),
            Confirmation {
                token,
                expires_at,
                effects: network_effects(effects),
            },
        ),
        recovered,
    )
}

pub(crate) fn network_effects(effects: &LaneNetworkEffects) -> NetworkEffects {
    NetworkEffects {
        from: setting(&effects.from),
        to: setting(&effects.to),
        ends_connections: effects.ends_connections,
        caller: effects.caller.map(|caller| match caller {
            LaneCallerEffect::StaysConnected => CallerEffect::StaysConnected,
            LaneCallerEffect::Reconnects => CallerEffect::Reconnects,
            LaneCallerEffect::Disconnected => CallerEffect::Disconnected,
        }),
        disconnects: effects
            .disconnects
            .iter()
            .map(|device| DeviceRef {
                id: device.id.clone(),
                name: device.name.clone(),
            })
            .collect(),
    }
}

fn setting(setting: &LaneNetworkSetting) -> NetworkSetting {
    NetworkSetting {
        enabled: setting.enabled,
        bind: setting.bind.clone(),
    }
}

fn describe(setting: &LaneNetworkSetting) -> String {
    match (&setting.bind, setting.enabled) {
        (Some(bind), true) => format!("on at {bind}"),
        _ => "off".to_owned(),
    }
}

/// The preview text. It names the server and the devices that lose their route, so it goes to
/// stdout, or to stderr before an interactive confirmation.
pub(crate) fn summary(server: &str, effects: &LaneNetworkEffects) -> String {
    let mut out = format!(
        "Change Network Access on {server} from {} to {}?\n",
        describe(&effects.from),
        describe(&effects.to)
    );
    if effects.ends_connections {
        out.push_str(
            "Every paired connection ends. A device reconnects only if the new setting serves the address it uses.\n",
        );
    }
    match effects.caller {
        Some(LaneCallerEffect::StaysConnected) => out.push_str("This device stays connected.\n"),
        Some(LaneCallerEffect::Reconnects) => {
            out.push_str("This device's connection ends, and it reconnects.\n");
        }
        Some(LaneCallerEffect::Disconnected) => out.push_str(
            "This device loses its connection and can't reconnect at its saved address.\n",
        ),
        None => {}
    }
    if !effects.disconnects.is_empty() {
        out.push_str("These connected devices lose their route:\n");
        for device in &effects.disconnects {
            out.push_str(&format!("  id:{}  {}\n", device.id, device.name));
        }
    }
    out
}

fn execute(
    context: &Context<'_>,
    access: NetworkAccess,
    routed: Routed,
    opened: Opened,
    pending: &Pending,
    token: String,
) -> Reply {
    let Opened {
        lane,
        session,
        recovered,
    } = opened;
    let action = LaneAction::Network { access };
    let wait = context.timeout(admin::wait(&action));
    let outcome = admin::execute(
        &lane,
        session,
        action,
        token,
        wait,
        pending.operations(),
        &mut super::dispatch,
        &crate::interrupt::settle_by,
    );
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            return Reply::new(
                &super::with_recovered(
                    Envelope::error(
                        COMMAND,
                        Some(routed.server),
                        super::lane_failure(error),
                        contract::Empty {},
                    ),
                    recovered,
                ),
                String::new(),
            );
        }
    };
    let settled = super::lifecycle::settle(ServiceAction::Network, outcome);
    let network = settled.failure.is_none().then(|| read_back(lane)).flatten();
    let status = context.command_line(&routed.server, "server status");
    let (envelope, text) = result(routed.server, settled, network, &status);
    Reply::new(&super::with_recovered(envelope, recovered), text)
}

/// The envelope and text of a confirmed change, from its settled flow and the Network Access
/// read back after it, if any. `status` is the command that reads the same server's status.
pub(crate) fn result(
    server: ServerRef,
    settled: Settled,
    network: Option<LaneNetworkSetting>,
    status: &str,
) -> (Envelope<ServerNetwork>, String) {
    let text = match (&settled.failure, &network) {
        (None, Some(network)) => format!("Network Access is now {}.\n", describe(network)),
        (None, None) => "Network Access changed.\n".to_owned(),
        (Some(failure), _) if failure.code == Code::OutcomeUnconfirmed => format!(
            "Check Network Access with: {status}\nIf this device can't reach {}, run spaceterm server status on that machine.\n",
            server.name
        ),
        (Some(_), _) => String::new(),
    };
    let data = ServerNetwork {
        network: network.as_ref().map(setting),
        operation: Some(settled.receipt.operation.clone()),
        receipt: Some(settled.receipt),
    };
    let envelope = match settled.failure {
        None => Envelope::ok(COMMAND, Some(server), data),
        Some(failure) => Envelope::error(COMMAND, Some(server), failure, data),
    };
    (envelope, text)
}

/// Network Access as the server reports it after a completed change, over a new connection.
/// `None` when this device can't read it in time, or after an interrupt.
fn read_back(lane: Lane) -> Option<LaneNetworkSetting> {
    if crate::interrupt::interrupted() {
        return None;
    }
    let deadline = Instant::now() + READ_BACK;
    let status = within(
        "spaceterm-network-read",
        &|| Some(crate::interrupt::limit(deadline)),
        move || {
            let mut session = lane.session().ok()?;
            match session.request(&LaneRequest::Status, deadline).ok()? {
                LaneReply::Status(status) => Some(status.network),
                _ => None,
            }
        },
    )??;
    Some(LaneNetworkSetting {
        enabled: status.enabled,
        bind: status.bind,
    })
}

#[cfg(test)]
mod tests;
