//! Content-free client failures and retry policy.

use spaceterm_protocol::ProtocolError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClientFailureClass {
    Unavailable,
    AuthorizationDenied,
    PinMismatch,
    Incompatible,
    MissingCapability,
    InvalidPeer,
    UnsafeEndpoint,
    StaleAuthority,
    TargetNotFound,
    ResourceLimit,
    StorageUnavailable,
    UnsafeStorage,
    InvalidStorage,
    UnknownSchema,
    RecoveryRequired,
    CounterExhausted,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetryDisposition {
    Automatic,
    Explicit,
    Recovery,
}

/// No native error or peer-provided value is retained in this error.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("client failure: {class:?}")]
pub struct ClientFailure {
    class: ClientFailureClass,
}

impl ClientFailure {
    pub const fn new(class: ClientFailureClass) -> Self {
        Self { class }
    }

    pub const fn class(self) -> ClientFailureClass {
        self.class
    }

    pub const fn retry(self) -> RetryDisposition {
        match self.class {
            ClientFailureClass::Unavailable => RetryDisposition::Automatic,
            ClientFailureClass::StorageUnavailable
            | ClientFailureClass::UnsafeStorage
            | ClientFailureClass::InvalidStorage
            | ClientFailureClass::UnknownSchema
            | ClientFailureClass::RecoveryRequired => RetryDisposition::Recovery,
            _ => RetryDisposition::Explicit,
        }
    }
}

impl From<ProtocolError> for ClientFailure {
    fn from(value: ProtocolError) -> Self {
        Self::new(match value {
            ProtocolError::Incompatible => ClientFailureClass::Incompatible,
            ProtocolError::PairingDenied => ClientFailureClass::AuthorizationDenied,
            ProtocolError::Authentication => ClientFailureClass::PinMismatch,
            ProtocolError::LimitExceeded => ClientFailureClass::ResourceLimit,
            ProtocolError::Exhausted => ClientFailureClass::CounterExhausted,
            _ => ClientFailureClass::InvalidPeer,
        })
    }
}

pub(crate) fn invalid_peer() -> ClientFailure {
    ClientFailure::new(ClientFailureClass::InvalidPeer)
}

pub(crate) fn resource_limit() -> ClientFailure {
    ClientFailure::new(ClientFailureClass::ResourceLimit)
}

pub(crate) fn exhausted() -> ClientFailure {
    ClientFailure::new(ClientFailureClass::CounterExhausted)
}
