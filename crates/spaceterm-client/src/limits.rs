//! Client admission limits. Constructors only reduce these defaults.

use std::time::Duration;

use crate::failure::{ClientFailure, resource_limit};

pub const MAX_ROUTE_BYTES: usize = 4096;
pub const DIAL_DEADLINE: Duration = Duration::from_secs(10);
pub const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(10);
pub const RECONCILIATION_DEADLINE: Duration = Duration::from_secs(15);
pub const RETRY_MINIMUM: Duration = Duration::from_millis(250);
pub const RETRY_MAXIMUM: Duration = Duration::from_secs(30);
pub const MAX_RESYNC_FAILURES: u8 = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClientLimits {
    pub saved_servers: usize,
    pub connection_actors: usize,
    pub organization_records: usize,
    pub organization_server_bytes: usize,
    pub organization_registry_bytes: usize,
}

impl Default for ClientLimits {
    fn default() -> Self {
        Self {
            saved_servers: 64,
            connection_actors: 16,
            organization_records: 32_768,
            organization_server_bytes: 64 * 1024 * 1024,
            organization_registry_bytes: 128 * 1024 * 1024,
        }
    }
}

impl ClientLimits {
    pub fn validate(self) -> Result<Self, ClientFailure> {
        let maximum = Self::default();
        for (value, ceiling) in [
            (self.saved_servers, maximum.saved_servers),
            (self.connection_actors, maximum.connection_actors),
            (self.organization_records, maximum.organization_records),
            (
                self.organization_server_bytes,
                maximum.organization_server_bytes,
            ),
            (
                self.organization_registry_bytes,
                maximum.organization_registry_bytes,
            ),
        ] {
            if value == 0 || value > ceiling {
                return Err(resource_limit());
            }
        }
        Ok(self)
    }
}
