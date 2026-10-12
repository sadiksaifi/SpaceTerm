//! Current actor-owned pairing lookup for ordinary and restricted handshakes.

use spaceterm_core::{
    identity::{ClientId, PairingId, ServerId},
    pairing::{PairingState, policy::PairingSnapshot},
};
use spaceterm_protocol::{
    ProtocolError,
    authentication::{Identity, PairingLookup, PairingRecord, Principal},
    pairing_adapter::EnrollmentLookup,
};

/// Reads current validated state, not a cached pre-revocation snapshot. Server
/// composition serializes readiness registration and final effect authority.
pub trait PairingSnapshotPort: Send + Sync {
    fn snapshot(&self) -> Result<PairingSnapshot, ProtocolError>;
}
pub struct PairingProtocolLookup<S> {
    source: S,
}
impl<S> PairingProtocolLookup<S> {
    pub const fn new(source: S) -> Self {
        Self { source }
    }
}
fn local(bytes: Identity) -> Result<u64, ProtocolError> {
    if bytes[..8] != [0; 8] {
        return Err(ProtocolError::Malformed);
    }
    let value = u64::from_be_bytes(
        bytes[8..]
            .try_into()
            .map_err(|_| ProtocolError::Malformed)?,
    );
    if value == 0 {
        return Err(ProtocolError::Malformed);
    }
    Ok(value)
}
impl<S: PairingSnapshotPort> PairingLookup for PairingProtocolLookup<S> {
    fn lookup(
        &self,
        server: Identity,
        pairing: Identity,
        client: Identity,
    ) -> Result<PairingRecord, ProtocolError> {
        let state = self.source.snapshot()?;
        if state.server().bytes() != server {
            return Err(ProtocolError::PairingDenied);
        }
        let server = ServerId::new(server);
        let id = PairingId::new(server, local(pairing)?).map_err(|_| ProtocolError::Malformed)?;
        let client_id =
            ClientId::new(server, local(client)?).map_err(|_| ProtocolError::Malformed)?;
        let a = state
            .authorization(id)
            .map_err(|_| ProtocolError::PairingDenied)?
            .parts();
        if !a.clients.iter().any(|c| c.parts().client.id == client_id) {
            return Err(ProtocolError::PairingDenied);
        }
        Ok(PairingRecord {
            principal: Principal {
                server: server.bytes(),
                device: a.record.device.bytes(),
                pairing,
                client,
            },
            device_public_key: a.keys.parts().pairing_authentication_public_key,
            active: a.record.state == PairingState::Authorized,
        })
    }
}
impl<S: PairingSnapshotPort> EnrollmentLookup for PairingProtocolLookup<S> {
    fn lookup(
        &self,
        ticket: &spaceterm_core::pairing::EnrollmentTicketId,
    ) -> Result<spaceterm_core::pairing::EnrollmentReservation, ProtocolError> {
        let state = self.source.snapshot()?;
        state
            .reservation(ticket)
            .cloned()
            .map_err(|_| ProtocolError::PairingDenied)
    }
}
impl<S: PairingSnapshotPort> crate::contracts::PairingAuthorizationPort
    for PairingProtocolLookup<S>
{
}
