//! Network Access choices and effects carried on the service lane.
use super::LaneDevice;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

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

impl LaneNetworkEffects {
    /// Whether the change leaves Network Access and every connection as they are.
    pub fn unchanged(&self) -> bool {
        self.from == self.to && !self.ends_connections
    }
}
