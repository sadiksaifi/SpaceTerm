//! Checked connection records and observable per-server state.

use std::{fmt, net::SocketAddr, sync::Arc, time::Duration};

use serde::{Deserialize, Serialize};
use spaceterm_core::{
    authority::{ConnectionBinding, OrganizationRevision, ServerEpoch},
    identity::{ClientId, DeviceId, PairingId, ServerId},
    pairing::{CredentialReference, LocalPairingGeneration, LocalPairingKey, LocalPairingKeyParts},
};
use spaceterm_protocol::authentication::ServerPin;

use crate::{
    failure::{ClientFailure, invalid_peer, resource_limit},
    limits::MAX_ROUTE_BYTES,
};

/// Saved reachability data. These values never establish local file authority.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RouteParts", into = "RouteParts")]
pub struct RouteDescription(RouteParts);

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
pub enum RouteParts {
    Ipc {
        directory: String,
        name: String,
    },
    Wss {
        address: SocketAddr,
        server_name: String,
    },
    Ssh {
        destination: String,
        remote_socket: String,
    },
}

impl fmt::Debug for RouteDescription {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RouteDescription(..)")
    }
}

impl RouteDescription {
    pub fn new(parts: RouteParts) -> Result<Self, ClientFailure> {
        let valid_text = |value: &str| !value.is_empty() && !value.chars().any(char::is_control);
        let (bytes, valid) = match &parts {
            RouteParts::Ipc { directory, name } => (
                directory.len().checked_add(name.len()),
                valid_text(directory)
                    && directory.starts_with('/')
                    && directory.split('/').all(|part| part != ".." && part != ".")
                    && !name.is_empty()
                    && name.len() <= 16
                    && name
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte)),
            ),
            RouteParts::Wss { server_name, .. } => (
                Some(server_name.len() + 32),
                valid_text(server_name) && server_name.len() <= 253,
            ),
            RouteParts::Ssh {
                destination,
                remote_socket,
            } => (
                destination.len().checked_add(remote_socket.len()),
                valid_text(destination)
                    && !destination.starts_with('-')
                    && valid_text(remote_socket)
                    && remote_socket.starts_with('/'),
            ),
        };
        if bytes.is_none_or(|bytes| bytes > MAX_ROUTE_BYTES) {
            return Err(resource_limit());
        }
        if !valid {
            return Err(invalid_peer());
        }
        Ok(Self(parts))
    }

    pub fn parts(&self) -> &RouteParts {
        &self.0
    }
}

impl TryFrom<RouteParts> for RouteDescription {
    type Error = ClientFailure;
    fn try_from(value: RouteParts) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}
impl From<RouteDescription> for RouteParts {
    fn from(value: RouteDescription) -> Self {
        value.0
    }
}

/// The exact original pin, principal, route, and native credential reference.
/// Construction checks comparison data; durable pairing verification is journal-owned.
#[derive(Clone, Eq, PartialEq)]
pub struct ConnectionTarget {
    pin: ServerPin,
    device: DeviceId,
    pairing: PairingId,
    client: ClientId,
    route: RouteDescription,
    credential: CredentialReference,
    incarnation: LocalPairingGeneration,
}

impl fmt::Debug for ConnectionTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ConnectionTarget(..)")
    }
}

impl ConnectionTarget {
    pub fn new(
        pin: ServerPin,
        device: DeviceId,
        pairing: PairingId,
        client: ClientId,
        route: RouteDescription,
        credential: CredentialReference,
        incarnation: LocalPairingGeneration,
    ) -> Result<Self, ClientFailure> {
        if pin.identity == [0; 16]
            || pin.public_key == [0; 32]
            || device.bytes() == [0; 16]
            || pairing.server().bytes() != pin.identity
            || client.server() != pairing.server()
        {
            return Err(invalid_peer());
        }
        Ok(Self {
            pin,
            device,
            pairing,
            client,
            route,
            credential,
            incarnation,
        })
    }

    pub fn pin(&self) -> ServerPin {
        self.pin
    }
    pub fn server(&self) -> ServerId {
        self.pairing.server()
    }
    pub fn device(&self) -> DeviceId {
        self.device
    }
    pub fn pairing(&self) -> PairingId {
        self.pairing
    }
    pub fn client(&self) -> ClientId {
        self.client
    }
    pub fn route(&self) -> &RouteDescription {
        &self.route
    }
    pub fn credential(&self) -> CredentialReference {
        self.credential
    }
    pub fn incarnation(&self) -> LocalPairingGeneration {
        self.incarnation
    }
    pub fn local_pairing(&self) -> Result<LocalPairingKey, ClientFailure> {
        LocalPairingKey::new(LocalPairingKeyParts {
            server: self.server(),
            pairing: self.pairing,
            generation: self.incarnation,
        })
        .map_err(|_| invalid_peer())
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct PairedServerBinding(Arc<ConnectionTarget>);

impl fmt::Debug for PairedServerBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PairedServerBinding(..)")
    }
}
impl PairedServerBinding {
    pub fn new(target: ConnectionTarget) -> Self {
        Self(Arc::new(target))
    }
    pub fn target(&self) -> &ConnectionTarget {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConnectingPhase {
    Dialing,
    Authenticating,
    Reconciling,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompatibilityExplanation {
    NoCommonMajor,
    CanonicalDomainRequired,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetryState {
    pub failures: u32,
    pub delay: Option<Duration>,
}

/// Authenticated optional capabilities, not authorization or release versions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConnectionCapabilities(u16);
impl ConnectionCapabilities {
    pub const NONE: Self = Self(0);
    const SUPPORTED: &[u32] = {
        use spaceterm_protocol::negotiation::*;
        &[
            CAPABILITY_NORMALIZED_CREATION_TOPOLOGY,
            CAPABILITY_CANONICAL_DOMAIN,
            CAPABILITY_SERVER_GIT,
            CAPABILITY_SERVER_DIRECTORIES,
            CAPABILITY_ORGANIZATION_COMPARISONS,
            CAPABILITY_TERMINAL_STREAMS,
            CAPABILITY_NATIVE_NOTIFICATIONS,
            CAPABILITY_APPLICATION_MOUSE,
            CAPABILITY_RUNTIME_METADATA,
            CAPABILITY_RUNTIME_METADATA_FRAGMENTS,
            CAPABILITY_NATIVE_PASTE_CORRELATION,
            CAPABILITY_ENHANCED_COPY,
            CAPABILITY_SERVER_MACHINE_FACTS,
            CAPABILITY_ACCOUNT_RUNTIME,
            CAPABILITY_REMOTE_NATIVE_LIFECYCLE,
        ]
    };
    pub(crate) fn optional_advertisement() -> &'static [u32] {
        // The first two capabilities are the registry's required domain baseline.
        &Self::SUPPORTED[2..]
    }
    pub(crate) fn from_advertisement(values: &[u32]) -> Self {
        let mut flags = 0;
        for (index, capability) in Self::SUPPORTED.iter().enumerate() {
            if values.contains(capability) {
                flags |= 1 << index;
            }
        }
        Self(flags)
    }
    pub fn supports(self, capability: u32) -> bool {
        Self::SUPPORTED
            .iter()
            .position(|installed| *installed == capability)
            .is_some_and(|index| self.0 & (1 << index) != 0)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConnectedInfo {
    pub server: ServerId,
    pub pairing: PairingId,
    pub client: ClientId,
    pub epoch: ServerEpoch,
    pub binding: ConnectionBinding,
    pub major: u32,
    pub limits: spaceterm_protocol::limits::NegotiatedLimits,
    pub capabilities: ConnectionCapabilities,
    pub organization_revision: OrganizationRevision,
    /// Local attempt, independent of the server's connection generation.
    pub attempt: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConnectionState {
    Connecting {
        phase: ConnectingPhase,
    },
    Connected {
        info: ConnectedInfo,
    },
    Offline {
        failure: ClientFailure,
        retry: RetryState,
    },
    Incompatible {
        explanation: CompatibilityExplanation,
    },
    Stopped,
}

#[derive(Clone, Debug)]
pub struct ServerSummary {
    pub server: ServerId,
    pub state: ConnectionState,
}

pub use crate::connection::queries::{PairedDevicePage, ServerQueries, ServerStatusBackend};

#[cfg(test)]
mod capability_tests {
    use super::*;
    #[test]
    fn native_runtime_metadata_copy_paste_and_facts_are_independent_connection_capabilities() {
        use spaceterm_protocol::negotiation::*;
        let features = [
            CAPABILITY_RUNTIME_METADATA_FRAGMENTS,
            CAPABILITY_NATIVE_PASTE_CORRELATION,
            CAPABILITY_ENHANCED_COPY,
            CAPABILITY_SERVER_MACHINE_FACTS,
            CAPABILITY_ACCOUNT_RUNTIME,
            CAPABILITY_REMOTE_NATIVE_LIFECYCLE,
        ];
        for mask in 0u8..64 {
            let advertised: Vec<u32> = features
                .iter()
                .enumerate()
                .filter_map(|(index, capability)| {
                    ((mask & (1 << index)) != 0).then_some(*capability)
                })
                .collect();
            let capabilities = ConnectionCapabilities::from_advertisement(&advertised);
            for (index, feature) in features.iter().enumerate() {
                assert_eq!(capabilities.supports(*feature), (mask & (1 << index)) != 0);
            }
        }
    }
    #[test]
    fn installed_terminal_capabilities_survive_connection_negotiation() {
        use spaceterm_protocol::negotiation::*;
        let installed = [
            CAPABILITY_ORGANIZATION_COMPARISONS,
            CAPABILITY_TERMINAL_STREAMS,
            CAPABILITY_NATIVE_NOTIFICATIONS,
        ];
        let capabilities = ConnectionCapabilities::from_advertisement(&installed);
        for capability in installed {
            assert!(capabilities.supports(capability));
        }
    }
}
