//! Versioned, bounded SpaceTerm wire contracts. No transport or async runtime is required.

pub mod authentication;
pub mod codec;
pub mod connection;
pub mod frame;
pub mod limits;
pub mod negotiation;
mod preflight;
mod semantics;

/// Generated wire DTOs. Receive untrusted bytes through the bounded connection codec,
/// not directly through `prost::Message::decode`.
pub mod wire {
    pub mod pairing {
        include!(concat!(env!("OUT_DIR"), "/spaceterm.pairing.rs"));
    }
    pub mod git {
        pub mod v1 {
            include!(concat!(env!("OUT_DIR"), "/spaceterm.git.v1.rs"));
        }
        pub mod v2 {
            include!(concat!(env!("OUT_DIR"), "/spaceterm.git.v2.rs"));
        }
    }
    pub mod directories {
        pub mod v1 {
            include!(concat!(env!("OUT_DIR"), "/spaceterm.directories.v1.rs"));
        }
        pub mod v2 {
            include!(concat!(env!("OUT_DIR"), "/spaceterm.directories.v2.rs"));
        }
    }
    pub mod common {
        include!(concat!(env!("OUT_DIR"), "/spaceterm.common.rs"));
    }
    pub mod v1 {
        include!(concat!(env!("OUT_DIR"), "/spaceterm.v1.rs"));
    }
    pub mod v2 {
        include!(concat!(env!("OUT_DIR"), "/spaceterm.v2.rs"));
    }
}

/// Failures contain classifications only, never peer-provided content or native errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ProtocolError {
    #[error("protocol limit exceeded")]
    LimitExceeded,
    #[error("malformed protocol message")]
    Malformed,
    #[error("incompatible protocol versions")]
    Incompatible,
    #[error("invalid protocol negotiation")]
    Negotiation,
    #[error("authentication failed")]
    Authentication,
    #[error("pairing is not authorized")]
    PairingDenied,
    #[error("unexpected authenticated message")]
    UnexpectedMessage,
    #[error("authenticated sequence mismatch")]
    Sequence,
    #[error("protocol counter exhausted")]
    Exhausted,
    #[error("signing service unavailable")]
    SigningUnavailable,
    #[error("secure random source unavailable")]
    RandomUnavailable,
}

pub mod convert;
pub mod pairing_adapter;
