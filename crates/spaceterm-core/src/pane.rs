//! Core Pane behavior and the Pane wire contract.
use crate::CoreError;

pub mod control;
pub mod run;
pub use spaceterm_protocol::pane::*;

impl From<Refusal> for CoreError {
    fn from(value: Refusal) -> Self {
        match value {
            Refusal::Capacity => Self::Capacity,
            Refusal::Controlled => Self::Controlled,
            Refusal::Missing => Self::SessionUnavailable,
            Refusal::Stale => Self::StaleControl,
            Refusal::Unsupported => Self::Unsupported,
            Refusal::Failed => Self::StartupFailed,
            Refusal::DirectoryUnavailable => Self::DirectoryUnavailable,
            Refusal::StorageUnavailable => Self::StorageUnavailable,
            Refusal::CleanupUnresolved => Self::CleanupUnresolved,
            Refusal::ServiceBusy => Self::ServiceBusy,
        }
    }
}
