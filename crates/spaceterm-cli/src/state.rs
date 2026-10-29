//! The CLI's own state under `clients/cli`: Saved Connections, and per route and identity the
//! client record, the Workspace edit journal, attachments and pending runs.
//!
//! ```text
//! clients/cli/
//!   connections.json, connections.lock
//!   routes/<local | 64-hex route>/
//!     base/            client.json (local route only), operations.json, operations.lock,
//!                      attachments.json, runs.json, inventory.json (paired routes only)
//!     agents/<name>/   the same files except client.json
//! ```
use crate::contract::{Code, RecoveredIdentity};
use spaceterm_server::protocol::AgentName;
use spaceterm_server::{Directory, Lock, LockWait, ServerError};
use std::path::PathBuf;
use std::time::Instant;

/// The writer lock's file in each identity directory.
pub const WRITER_LOCK: &str = "operations.lock";

pub struct CliState {
    root: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RouteKey {
    Local,
    Paired([u8; 32]),
}

impl RouteKey {
    /// The route's directory name: `local`, or the route ID as 64 hex.
    pub fn directory_name(&self) -> String {
        match self {
            Self::Local => "local".to_owned(),
            Self::Paired(route) => route.iter().map(|byte| format!("{byte:02x}")).collect(),
        }
    }
}

/// Held by a mutating command from journal open through acknowledgement.
pub struct WriterGuard {
    _lock: Lock,
}

/// An identity of one route with unsettled journal entries or runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeldIdentity {
    pub identity: RecoveredIdentity,
    pub operations: u32,
    pub runs: u32,
}

impl CliState {
    pub fn open(identity: &str) -> Result<Self, ServerError> {
        Ok(Self {
            root: spaceterm_server::cli_state_root(identity)?,
        })
    }

    fn identity_path(&self, route: &RouteKey, agent: Option<&AgentName>) -> PathBuf {
        let route = self.root.join("routes").join(route.directory_name());
        match agent {
            None => route.join("base"),
            Some(agent) => route.join("agents").join(agent.as_str()),
        }
    }

    /// The identity's private directory, created on first use.
    pub fn identity_dir(
        &self,
        route: &RouteKey,
        agent: Option<&AgentName>,
    ) -> Result<Directory, ServerError> {
        Directory::open(&self.identity_path(route, agent), true)
    }

    /// The identity's writer lock. Contention waits until `deadline`, then fails with
    /// `identity_busy`.
    pub fn writer(
        &self,
        route: &RouteKey,
        agent: Option<&AgentName>,
        deadline: Instant,
    ) -> Result<WriterGuard, Code> {
        let directory = self
            .identity_dir(route, agent)
            .map_err(|_| Code::StorageUnavailable)?;
        let limit = deadline.saturating_duration_since(Instant::now());
        match directory.wait_lock(WRITER_LOCK, limit, None) {
            Ok(LockWait::Locked(lock)) => Ok(WriterGuard { _lock: lock }),
            Ok(LockWait::Held | LockWait::Cancelled) => Err(Code::IdentityBusy),
            Err(_) => Err(Code::StorageUnavailable),
        }
    }

    /// Every identity of `route` that holds unsettled outcomes.
    pub fn held(&self, _route: &RouteKey) -> Result<Vec<HeldIdentity>, ServerError> {
        Err(ServerError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identities_live_under_their_route() {
        let state = CliState {
            root: PathBuf::from("/state/clients/cli"),
        };
        let agent: AgentName = "agent-1".parse().unwrap();
        assert_eq!(
            state.identity_path(&RouteKey::Local, None),
            PathBuf::from("/state/clients/cli/routes/local/base")
        );
        assert_eq!(
            state.identity_path(&RouteKey::Paired([0xab; 32]), Some(&agent)),
            PathBuf::from(format!(
                "/state/clients/cli/routes/{}/agents/agent-1",
                "ab".repeat(32)
            ))
        );
    }
}
