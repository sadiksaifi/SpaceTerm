//! The service-operation ledger: one record per remote Stop, Restart or Update, from admission
//! through its outcome. It lives in `server/service-operations.json` under
//! `service-operations.lock`, so an outcome survives the server that admitted it.
use crate::ServerError;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// The most unacknowledged records a ledger keeps. A full ledger refuses `Execute`.
pub const MAX_RECORDS: usize = 16;

/// Names one service operation. The client chooses it, so a repeated `Execute` dispatches
/// nothing new.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct OperationId(pub [u8; 16]);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    Stop,
    Restart,
    Update,
    Return,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Actor {
    pub pairing: Option<crate::direct::PairingId>,
    pub client: crate::hierarchy::Id,
}

/// Why an admitted operation was never performed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NotPerformed {
    ServerRestarted,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum State {
    Admitted,
    Launched,
    Running,
    Done,
    /// `code` is a code from the CLI output contract.
    Failed {
        code: String,
    },
    NotPerformed(NotPerformed),
    Unresolved,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub id: OperationId,
    pub actor: Actor,
    pub server: crate::hierarchy::Id,
    pub boot: crate::hierarchy::Id,
    pub action: Action,
    pub observation_digest: [u8; 32],
    pub candidate: Option<[u8; 32]>,
    pub state: State,
    /// Seconds since the Unix epoch.
    pub updated_at: u64,
}

/// One account's ledger. Every change is written and synced before it is reported.
pub struct Ledger {
    #[allow(
        dead_code,
        reason = "the ledger file is read and written by the service lane"
    )]
    path: PathBuf,
}

impl Ledger {
    pub fn open(_identity: &str) -> Result<Self, ServerError> {
        Err(ServerError::Unsupported)
    }

    /// Persists `record` as `Admitted`. A repeated ID returns the existing record.
    pub fn admit(&mut self, _record: Record) -> Result<Record, ServerError> {
        Err(ServerError::Unsupported)
    }

    pub fn mark(&mut self, _operation: &OperationId, _state: State) -> Result<(), ServerError> {
        Err(ServerError::Unsupported)
    }

    pub fn resolve(&self, _operation: &OperationId) -> Result<Option<Record>, ServerError> {
        Err(ServerError::Unsupported)
    }

    /// Deletes a record, only for its own actor.
    pub fn acknowledge(
        &mut self,
        _operation: &OperationId,
        _actor: &Actor,
    ) -> Result<(), ServerError> {
        Err(ServerError::Unsupported)
    }

    /// Settles records a previous server left behind, before it accepts connections.
    pub fn recover_at_start(&mut self, _boot: &crate::hierarchy::Id) -> Result<(), ServerError> {
        Err(ServerError::Unsupported)
    }
}
