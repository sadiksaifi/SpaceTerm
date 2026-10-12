//! Durable remote-access comparison data. This module never opens a listener.

use std::fmt;
use std::net::SocketAddr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::identity::ServerId;

pub const MAX_REMOTE_BIND_ENDPOINTS: usize = 8;

/// Remote-access policy conversion failure, without endpoint values.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum RemoteAccessError {
    #[error("enabled remote access requires an established server identity")]
    InvalidServer,
    #[error("enabled remote access requires an explicit bind endpoint")]
    EmptyEnablement,
    #[error("remote-access bind scope exceeds the endpoint limit")]
    TooManyEndpoints,
    #[error("remote-access bind scope contains duplicate endpoints")]
    DuplicateEndpoint,
}

/// Exact owning Server and IPv4/IPv6 bind endpoints. An empty scope is Disabled.
/// Wildcards and port zero must be explicit. IPv6 binding remains IPv6-only in
/// the host adapter; an IPv6 wildcard never implicitly enables IPv4.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "RemoteAccessParts", into = "RemoteAccessParts")]
pub struct RemoteAccessPolicy {
    server: ServerId,
    endpoints: Vec<SocketAddr>,
}

#[derive(Clone, Deserialize, Serialize)]
struct RemoteAccessParts {
    server: ServerId,
    endpoints: Vec<SocketAddr>,
}

impl RemoteAccessPolicy {
    pub fn disabled(server: ServerId) -> Self {
        Self {
            server,
            endpoints: Vec::new(),
        }
    }

    pub fn enabled(
        server: ServerId,
        endpoints: Vec<SocketAddr>,
    ) -> Result<Self, RemoteAccessError> {
        if endpoints.is_empty() {
            return Err(RemoteAccessError::EmptyEnablement);
        }
        Self::from_parts(server, endpoints)
    }

    fn from_parts(server: ServerId, endpoints: Vec<SocketAddr>) -> Result<Self, RemoteAccessError> {
        if !endpoints.is_empty() && server.bytes() == [0; 16] {
            return Err(RemoteAccessError::InvalidServer);
        }
        if endpoints.len() > MAX_REMOTE_BIND_ENDPOINTS {
            return Err(RemoteAccessError::TooManyEndpoints);
        }
        if endpoints
            .iter()
            .enumerate()
            .any(|(index, endpoint)| endpoints[..index].contains(endpoint))
        {
            return Err(RemoteAccessError::DuplicateEndpoint);
        }
        Ok(Self { server, endpoints })
    }

    pub const fn server(&self) -> ServerId {
        self.server
    }
    pub fn is_enabled(&self) -> bool {
        !self.endpoints.is_empty()
    }
    pub fn endpoints(&self) -> &[SocketAddr] {
        &self.endpoints
    }
}

impl fmt::Debug for RemoteAccessPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RemoteAccessPolicy")
            .field("server", &self.server)
            .field("enabled", &self.is_enabled())
            .field("endpoint_count", &self.endpoints.len())
            .finish()
    }
}

impl TryFrom<RemoteAccessParts> for RemoteAccessPolicy {
    type Error = RemoteAccessError;

    fn try_from(parts: RemoteAccessParts) -> Result<Self, Self::Error> {
        Self::from_parts(parts.server, parts.endpoints)
    }
}

impl From<RemoteAccessPolicy> for RemoteAccessParts {
    fn from(value: RemoteAccessPolicy) -> Self {
        Self {
            server: value.server,
            endpoints: value.endpoints,
        }
    }
}
