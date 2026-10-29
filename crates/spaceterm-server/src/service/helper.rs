//! The transient service helper. It runs one admitted service operation outside the server it
//! stops or replaces: `spaceterm server provision service-run --operation ID --identity X`.
use super::operations::OperationId;
use crate::ServerError;

/// Starts the helper for `op` from the approved runtime, detached from this server.
pub fn launch(_identity: &str, _op: &OperationId) -> Result<(), ServerError> {
    Err(ServerError::Unsupported)
}

/// The helper's own work: takes the service lock, runs the action and records its outcome.
pub fn run(_identity: &str, _op: &OperationId) -> Result<(), ServerError> {
    Err(ServerError::Unsupported)
}
