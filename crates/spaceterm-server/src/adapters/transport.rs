//! Content-free transport and protocol failures at the server boundary.

use crate::{
    contracts::{ServerFailure, ServerFailureClass},
    failure::failure,
};
use spaceterm_host::transport::{PeerFacts, TransportError, TransportRoute};
use spaceterm_protocol::ProtocolError;

pub fn check_peer(facts: PeerFacts, owning_uid: u32) -> Result<(), ServerFailure> {
    if facts.route == TransportRoute::PrivateIpc && facts.local_uid != Some(owning_uid) {
        return Err(failure(ServerFailureClass::Authentication));
    }
    Ok(())
}

pub fn transport_failure(value: TransportError) -> ServerFailure {
    failure(match value {
        TransportError::PeerAccountRejected
        | TransportError::TlsIdentityRejected
        | TransportError::HandshakeExpired => ServerFailureClass::Authentication,
        TransportError::InvalidConfiguration
        | TransportError::InvalidFrame
        | TransportError::WebSocketRejected => ServerFailureClass::ScopeMismatch,
        TransportError::MessageTooLarge => ServerFailureClass::Capacity,
        TransportError::UnsafeEndpoint => ServerFailureClass::NativeUnavailable,
        _ => ServerFailureClass::RuntimeUnavailable,
    })
}

pub fn protocol_failure(value: ProtocolError) -> ServerFailure {
    failure(match value {
        ProtocolError::Authentication | ProtocolError::PairingDenied | ProtocolError::Sequence => {
            ServerFailureClass::Authentication
        }
        ProtocolError::LimitExceeded => ServerFailureClass::Capacity,
        ProtocolError::Exhausted => ServerFailureClass::Exhausted,
        ProtocolError::SigningUnavailable | ProtocolError::RandomUnavailable => {
            ServerFailureClass::NativeUnavailable
        }
        _ => ServerFailureClass::ScopeMismatch,
    })
}
