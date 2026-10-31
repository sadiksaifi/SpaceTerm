//! Network Access changes over the service lane. A change can end the connection that asked for
//! it, so its preview reports what becomes of the caller and of every connected Paired Device.
//!
//! The host plans a change under its direct-network lock: Network Access now and after the
//! change, whether the listener starts over and so ends every paired connection, and which
//! connected devices the new listener no longer serves. A device keeps its route when the new
//! listener serves the server address its connection reached. A paired caller may not make a
//! change that leaves no connected device a route: that change requires the server's own machine.
//!
//! The lane binds a preview's token to the plan, admits the change in the ledger, writes
//! `Accepted`, then plans again and applies the change in-process. No helper runs and no Terminal
//! Session ends, so the change sets no membership fence. A change that turns access on settles as
//! done only once its listener accepts connections.
use super::LaneDevice;
use crate::direct::PairingId;
use crate::hierarchy::Id;
use crate::service::operations::Actor;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::time::Duration;

/// How long an applied change may wait for its listener to accept. A narrowing binds only after
/// the listener it replaces retires, within [`crate::connection::quic::RETIRE_LIMIT`], and the
/// bind itself may then wait up to `STALL_AFTER` for a socket that just closed.
pub const LISTEN_LIMIT: Duration = Duration::from_secs(
    crate::connection::quic::RETIRE_LIMIT.as_secs()
        + crate::protocol::limits::STALL_AFTER.as_secs()
        + 2,
);

/// Whether Network Access accepts connections, as a change waits for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListenerProgress {
    Listening,
    /// Waiting to bind, as while the listener it replaces retires.
    Starting,
    /// Off, or the listener failed.
    Stopped,
}

/// One reading of the host's listener.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ListenerView {
    /// The host's listener generation. Every policy change that replaces the listener starts a
    /// new one, so a later reading of the same generation is the listener a change applied.
    pub generation: [u8; 32],
    pub progress: ListenerProgress,
    /// Where it listens, or waits to.
    pub address: Option<SocketAddr>,
}

/// What a later reading shows of the listener a change applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Followed {
    /// That listener accepts connections.
    Accepting,
    /// That listener still waits to bind.
    Waiting,
    /// It stopped, or a later policy change replaced it before it accepted.
    Lost,
}

impl ListenerView {
    /// What `now` shows of the listener this reading was taken of, as a change applied it. A
    /// reading of another generation or address is a replacement, which proves nothing about
    /// the change, even when it listens.
    pub fn follow(&self, now: &ListenerView) -> Followed {
        if now.generation != self.generation || now.address != self.address {
            return Followed::Lost;
        }
        match now.progress {
            ListenerProgress::Listening => Followed::Accepting,
            ListenerProgress::Starting => Followed::Waiting,
            ListenerProgress::Stopped => Followed::Lost,
        }
    }
}

/// The Network Access a change asks for. `All` listens on every interface at the current port.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkAccess {
    Off,
    All,
    Address(SocketAddr),
}

/// Network Access before or after a change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneNetworkSetting {
    pub enabled: bool,
    pub bind: Option<String>,
}

/// What a change does to the paired connection that asked for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneCallerEffect {
    /// The listener keeps its connections.
    StaysConnected,
    /// The connection ends, and the new listener serves the address it reached.
    Reconnects,
    /// The connection ends, and the new listener does not serve the address it reached.
    Disconnected,
}

/// Mirrors the confirmation `effects` of `server.network`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneNetworkEffects {
    pub from: LaneNetworkSetting,
    pub to: LaneNetworkSetting,
    /// The listener starts over, which ends every paired connection, including the caller's.
    pub ends_connections: bool,
    /// `None` for a caller on the server's own machine.
    pub caller: Option<LaneCallerEffect>,
    /// Connected Paired Devices whose route the change removes, sorted by ID.
    pub disconnects: Vec<LaneDevice>,
}

/// The paired connection a change was asked on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NetworkCaller {
    pub pairing: PairingId,
    /// The server address the connection reached, when the system reported it.
    pub route: Option<SocketAddr>,
}

impl NetworkCaller {
    /// The caller of a lane connection: `None` over account IPC.
    pub fn of(actor: &Actor, route: Option<SocketAddr>) -> Option<Self> {
        actor.pairing.map(|pairing| Self { pairing, route })
    }
}

/// A change as the host planned it under its direct-network lock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetworkPlan {
    pub from: LaneNetworkSetting,
    pub to: LaneNetworkSetting,
    /// The listener generation the plan was made against.
    pub generation: [u8; 32],
    pub ends_connections: bool,
    pub caller: Option<LaneCallerEffect>,
    /// Sorted by ID.
    pub disconnects: Vec<LaneDevice>,
    /// Connected Paired Devices whose route the change keeps.
    pub kept: usize,
}

impl LaneNetworkEffects {
    /// Whether the change leaves Network Access and every connection as they are.
    pub fn unchanged(&self) -> bool {
        self.from == self.to && !self.ends_connections
    }
}

impl NetworkPlan {
    /// A paired caller may not leave every connected device without a route. The caller is
    /// connected, so a change that keeps no route ends Network Access for every device the
    /// server can see, including the one that asked.
    pub fn refused_for(&self, caller: Option<&NetworkCaller>) -> bool {
        caller.is_some() && self.kept == 0
    }

    pub fn effects(&self) -> LaneNetworkEffects {
        LaneNetworkEffects {
            from: self.from.clone(),
            to: self.to.clone(),
            ends_connections: self.ends_connections,
            caller: self.caller,
            disconnects: self.disconnects.clone(),
        }
    }

    /// The digest a preview's token binds: the server run, the request, both settings, the
    /// listener generation and every effect the preview reported.
    pub(crate) fn observation(&self, server: &Id, boot: &Id, action: &[u8]) -> [u8; 32] {
        let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
        digest.update(b"spaceterm-service-lane-network-1\0");
        digest.update(server);
        digest.update(boot);
        digest.update(&(action.len() as u64).to_be_bytes());
        digest.update(action);
        for setting in [&self.from, &self.to] {
            digest.update(&[u8::from(setting.enabled)]);
            let bind = setting.bind.as_deref().unwrap_or_default();
            digest.update(&(bind.len() as u64).to_be_bytes());
            digest.update(bind.as_bytes());
        }
        digest.update(&self.generation);
        digest.update(&[u8::from(self.ends_connections)]);
        digest.update(&[match self.caller {
            None => 0,
            Some(LaneCallerEffect::StaysConnected) => 1,
            Some(LaneCallerEffect::Reconnects) => 2,
            Some(LaneCallerEffect::Disconnected) => 3,
        }]);
        for device in &self.disconnects {
            digest.update(device.id.as_bytes());
        }
        let mut out = [0; 32];
        out.copy_from_slice(digest.finish().as_ref());
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setting(enabled: bool, bind: &str) -> LaneNetworkSetting {
        LaneNetworkSetting {
            enabled,
            bind: Some(bind.to_owned()),
        }
    }

    fn plan(kept: usize) -> NetworkPlan {
        NetworkPlan {
            from: setting(true, "0.0.0.0:9231"),
            to: setting(true, "192.0.2.4:9231"),
            generation: [3; 32],
            ends_connections: true,
            caller: Some(LaneCallerEffect::Reconnects),
            disconnects: Vec::new(),
            kept,
        }
    }

    #[test]
    fn only_a_paired_caller_is_refused_a_change_that_keeps_no_route() {
        let caller = NetworkCaller {
            pairing: [7; 32],
            route: None,
        };
        assert!(plan(0).refused_for(Some(&caller)));
        assert!(!plan(1).refused_for(Some(&caller)));
        assert!(!plan(0).refused_for(None));
    }

    #[test]
    fn the_observation_binds_the_request_and_every_reported_effect() {
        let base = plan(1).observation(&[1; 32], &[2; 32], b"all");
        assert_eq!(base, plan(1).observation(&[1; 32], &[2; 32], b"all"));
        assert_ne!(base, plan(1).observation(&[1; 32], &[2; 32], b"off"));
        assert_ne!(base, plan(1).observation(&[1; 32], &[9; 32], b"all"));
        let mut other = plan(1);
        other.generation = [4; 32];
        assert_ne!(base, other.observation(&[1; 32], &[2; 32], b"all"));
        let mut other = plan(1);
        other.caller = Some(LaneCallerEffect::Disconnected);
        assert_ne!(base, other.observation(&[1; 32], &[2; 32], b"all"));
        let mut other = plan(1);
        other.disconnects.push(LaneDevice {
            id: "ab".repeat(32),
            name: "Laptop".into(),
            fingerprint: String::new(),
            last_connected_at: None,
        });
        assert_ne!(base, other.observation(&[1; 32], &[2; 32], b"all"));
    }

    #[test]
    fn only_the_applied_listener_settles_a_change() {
        let at: SocketAddr = "127.0.0.1:9231".parse().unwrap();
        let reading = |generation, progress, address| ListenerView {
            generation: [generation; 32],
            progress,
            address,
        };
        let applied = reading(1, ListenerProgress::Starting, Some(at));
        assert_eq!(applied.follow(&applied), Followed::Waiting);
        assert_eq!(
            applied.follow(&reading(1, ListenerProgress::Listening, Some(at))),
            Followed::Accepting
        );
        assert_eq!(
            applied.follow(&reading(1, ListenerProgress::Stopped, Some(at))),
            Followed::Lost
        );
        // A later change's listener accepts, but not the one this change applied.
        let every = Some("0.0.0.0:9231".parse().unwrap());
        assert_eq!(
            applied.follow(&reading(2, ListenerProgress::Listening, every)),
            Followed::Lost
        );
        assert_eq!(
            applied.follow(&reading(2, ListenerProgress::Listening, Some(at))),
            Followed::Lost
        );
        assert_eq!(
            applied.follow(&reading(1, ListenerProgress::Listening, every)),
            Followed::Lost
        );
    }

    #[test]
    fn network_requests_keep_their_wire_form() {
        for (access, value) in [
            (NetworkAccess::Off, serde_json::json!("off")),
            (NetworkAccess::All, serde_json::json!("all")),
            (
                NetworkAccess::Address("192.0.2.4:9231".parse().unwrap()),
                serde_json::json!({ "address": "192.0.2.4:9231" }),
            ),
        ] {
            assert_eq!(serde_json::to_value(access).unwrap(), value);
            let request = super::super::LaneRequest::Execute {
                action: super::super::LaneAction::Network { access },
                token: "c1-00".into(),
                operation: "op".into(),
            };
            assert_eq!(
                serde_json::to_value(&request).unwrap(),
                serde_json::json!({
                    "request": "execute", "action": { "action": "network", "access": value },
                    "token": "c1-00", "operation": "op",
                })
            );
        }
    }
}
