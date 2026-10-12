//! Content-free failure mappings at composition boundaries.

pub use crate::contracts::{ServerFailure, ServerFailureClass};
use spaceterm_core::control_policy::ControlPolicyError;
use spaceterm_core::operation_ledger_contracts::{LedgerFailure, LedgerFailureClass};
use spaceterm_core::startup_policy::StartupPolicyError;
use spaceterm_core::storage::StoreFailure;

pub const fn failure(class: ServerFailureClass) -> ServerFailure {
    ServerFailure {
        class,
        expected: None,
        actual: None,
        limit: None,
    }
}
pub fn store_failure(value: StoreFailure) -> ServerFailure {
    failure(match value {
        StoreFailure::Locked | StoreFailure::Unavailable | StoreFailure::Io => {
            ServerFailureClass::StorageUnavailable
        }
        StoreFailure::Exhausted => ServerFailureClass::Exhausted,
        _ => ServerFailureClass::RecoveryRequired,
    })
}
pub fn ledger_failure(value: LedgerFailure) -> ServerFailure {
    let class = match value.class {
        LedgerFailureClass::ScopeMismatch | LedgerFailureClass::PurposeMismatch => {
            ServerFailureClass::ScopeMismatch
        }
        LedgerFailureClass::QuotaExceeded
        | LedgerFailureClass::StreamLimit
        | LedgerFailureClass::PreparationBusy => ServerFailureClass::Capacity,
        LedgerFailureClass::CounterExhausted => ServerFailureClass::Exhausted,
        LedgerFailureClass::RecoveryRequired | LedgerFailureClass::InvalidDurableState => {
            ServerFailureClass::RecoveryRequired
        }
        _ => ServerFailureClass::StaleAuthority,
    };
    ServerFailure {
        class,
        expected: value.metadata.expected,
        actual: value.metadata.actual,
        limit: value.metadata.limit,
    }
}
pub fn control_failure(value: ControlPolicyError) -> ServerFailure {
    failure(match value {
        ControlPolicyError::ScopeMismatch => ServerFailureClass::ScopeMismatch,
        ControlPolicyError::Exhausted => ServerFailureClass::Exhausted,
        ControlPolicyError::InvalidState => ServerFailureClass::RecoveryRequired,
        _ => ServerFailureClass::StaleAuthority,
    })
}
pub fn startup_failure(value: StartupPolicyError) -> ServerFailure {
    failure(match value {
        StartupPolicyError::ScopeMismatch => ServerFailureClass::ScopeMismatch,
        StartupPolicyError::AdmissionsClosed
        | StartupPolicyError::AlreadyStarted
        | StartupPolicyError::InvalidGeometry => ServerFailureClass::Capacity,
        StartupPolicyError::StaleAuthority => ServerFailureClass::StaleAuthority,
    })
}
