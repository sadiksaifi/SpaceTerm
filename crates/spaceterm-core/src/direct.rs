//! Deliberately enabled QUIC access, exact device enrollment and restricted revocation.
//! Native credential access and connection persistence belong to the calling client.
use crate::{CoreError, pane::control::ClientName};
pub use spaceterm_protocol::direct::{
    ClaimId, ComparisonCode, DeviceInfo, DirectError, DirectStatus, ListenerState, MachinePin,
    Management, ManagementReply, NetworkAddresses, NetworkPolicy, NetworkRestore, OperationId,
    PairedDevice, PairingClaim, PairingId, RestoredAccess, RevocationReceipt, default_port,
};

pub(crate) mod host;
mod network;
pub(crate) mod storage;
pub(crate) use storage::{forget, observe};
#[cfg(test)]
pub(crate) mod tests;
mod wire;

pub(super) fn setup_generation(operation: &[u8; 32]) -> u64 {
    let mut generation = [0; 8];
    generation.copy_from_slice(&operation[..8]);
    u64::from_be_bytes(generation)
}

pub use spaceterm_client::direct::{
    DeviceCredential, DirectConnection, DirectConnector, Endpoint, Enrollment, Invitation, revoke,
    revoke_until,
};
use spaceterm_client::direct::{client, crypto, quic};
