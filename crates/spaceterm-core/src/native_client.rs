//! Account-owned storage and system adapters for the paired client.
use crate::{
    MachineConnection,
    computer_name::{ComputerName, SystemComputerName},
};
use spaceterm_client::direct::{DirectError, MachinePin, PairingId, RevocationReceipt};
use spaceterm_client::records::Records;
use spaceterm_client::{
    AccountRevocation, CredentialStore, DeviceName, DirectConnections, UnpairKey,
};
use std::sync::Arc;

mod connection_storage;
pub mod credentials;
pub mod migration;

pub use crate::client::PairedClients;
pub use connection_storage::connection_store;

impl AccountRevocation for MachineConnection {
    fn revoke(
        &self,
        pin: &MachinePin,
        operation: UnpairKey,
        target: PairingId,
    ) -> Result<RevocationReceipt, DirectError> {
        if self.direct_status()?.pin != *pin {
            return Err(DirectError::WrongMachine);
        }
        self.revoke_device(operation, target)
    }
}

/// Constructs the registry using this computer's device name and route signal.
pub fn paired_routes(
    identity: &str,
    records: Arc<dyn Records>,
    credentials: Arc<dyn CredentialStore>,
) -> (DirectConnections, Option<Arc<PairedClients>>) {
    let local = MachineConnection::for_identity(identity).ok();
    let device = local.clone();
    let name: DeviceName = Arc::new(move || {
        device.as_ref().map_or_else(
            || {
                let computer = SystemComputerName.computer_name();
                spaceterm_client::pane::control::ClientName::for_computer(computer.as_deref(), None)
            },
            MachineConnection::client_name,
        )
    });
    let routes = DirectConnections::with_credentials(
        identity,
        records,
        credentials,
        name,
        Arc::new(crate::connection::route_signal::client_signal),
        local
            .as_ref()
            .map(|local| Arc::new(local.clone()) as Arc<dyn AccountRevocation>),
    );
    with_presentation(routes, local)
}

#[cfg(feature = "test-support")]
pub fn with_paired_presentation(
    identity: &str,
    routes: DirectConnections,
) -> (DirectConnections, Option<Arc<PairedClients>>) {
    let presentation = MachineConnection::for_identity(identity)
        .ok()
        .map(|base| Arc::new(PairedClients::new(base)));
    (routes, presentation)
}

fn with_presentation(
    routes: DirectConnections,
    local: Option<MachineConnection>,
) -> (DirectConnections, Option<Arc<PairedClients>>) {
    let presentation = local.map(|local| Arc::new(PairedClients::new(local)));
    if let Some(presentation) = &presentation {
        let presentation = Arc::clone(presentation);
        routes.on_route_removed(Arc::new(move |route| {
            let _ = presentation.remove_route(route);
        }));
    }
    (routes, presentation)
}
