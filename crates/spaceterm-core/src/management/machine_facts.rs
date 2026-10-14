//! Owning-server comparisons only. These facts never grant client-local authority.

use super::StatusError;
use serde::{Deserialize, Serialize};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};

pub const MAX_SERVER_BIND_SCOPES: usize = 2;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum ServerPlatform {
    #[default]
    Unknown,
    MacOs,
    Linux,
}
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum ServerPermissionState {
    #[default]
    Unknown,
    Granted,
    NotGranted,
    NotNeeded,
    Unavailable,
}
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum ServerAccessibilityNaming {
    #[default]
    Unknown,
    Accessibility,
    DeviceControl,
}
/// Fixed permission rows, without native error text or privacy database data.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ServerPermissionFacts {
    pub microphone: ServerPermissionState,
    pub screen_recording: ServerPermissionState,
    pub accessibility: ServerPermissionState,
    pub accessibility_naming: ServerAccessibilityNaming,
}
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum MachineFactsAvailability {
    #[default]
    Unknown,
    Available,
    Unavailable,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum ServerBindScope {
    Loopback,
    AllInterfaces,
}
/// An exact IPv4/IPv6 offer, not a listener or retained remote-access policy.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ServerBindScopeOffer {
    pub scope: ServerBindScope,
    pub endpoints: Vec<SocketAddr>,
}
/// Identity, epoch, and revision come exclusively from the enclosing ServerStatus.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ServerMachineFacts {
    pub platform: ServerPlatform,
    pub permissions: ServerPermissionFacts,
    pub bind_scope_availability: MachineFactsAvailability,
    pub bind_scopes: Vec<ServerBindScopeOffer>,
}
impl ServerMachineFacts {
    pub(super) fn validate(&self) -> Result<(), StatusError> {
        if self.bind_scopes.len() > MAX_SERVER_BIND_SCOPES {
            return Err(StatusError::Limit);
        }
        if match self.bind_scope_availability {
            MachineFactsAvailability::Available => self.bind_scopes.is_empty(),
            MachineFactsAvailability::Unknown | MachineFactsAvailability::Unavailable => {
                !self.bind_scopes.is_empty()
            }
        } {
            return Err(StatusError::InvalidBinding);
        }
        let mut scopes = std::collections::BTreeSet::new();
        for offer in &self.bind_scopes {
            if offer.endpoints.len() > crate::remote_access::MAX_REMOTE_BIND_ENDPOINTS {
                return Err(StatusError::Limit);
            }
            if !scopes.insert(offer.scope) || offer.endpoints.len() != 2 {
                return Err(StatusError::InvalidBinding);
            }
            let port = offer.endpoints[0].port();
            if port == 0 {
                return Err(StatusError::InvalidBinding);
            }
            let (v4, v6) = match offer.scope {
                ServerBindScope::Loopback => (Ipv4Addr::LOCALHOST, Ipv6Addr::LOCALHOST),
                ServerBindScope::AllInterfaces => (Ipv4Addr::UNSPECIFIED, Ipv6Addr::UNSPECIFIED),
            };
            let expected = [SocketAddr::from((v4, port)), SocketAddr::from((v6, port))];
            if offer.endpoints[0] == offer.endpoints[1]
                || !offer
                    .endpoints
                    .iter()
                    .all(|endpoint| expected.contains(endpoint))
            {
                return Err(StatusError::InvalidBinding);
            }
        }
        Ok(())
    }
}
