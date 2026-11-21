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

impl AccountRevocation for PairedClients {
    fn revoke(
        &self,
        pin: &MachinePin,
        operation: UnpairKey,
        target: PairingId,
    ) -> Result<RevocationReceipt, DirectError> {
        self.account()?.revoke(pin, operation, target)
    }
}

/// Constructs the portable registry and its native account context without resolving the account.
pub fn paired_routes(
    identity: &str,
    records: Arc<dyn Records>,
    credentials: Arc<dyn CredentialStore>,
) -> (DirectConnections, Arc<PairedClients>) {
    let presentation = paired_presentation(identity);
    let device = Arc::clone(&presentation);
    let name: DeviceName = Arc::new(move || {
        device.account().map_or_else(
            |_| {
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
        Some(presentation.clone()),
    );
    let cleanup = Arc::clone(&presentation);
    routes.on_route_removed(Arc::new(move |route| {
        let _ = cleanup.remove_route(route);
    }));
    (routes, presentation)
}

fn paired_presentation(identity: &str) -> Arc<PairedClients> {
    let identity = identity.to_owned();
    Arc::new(PairedClients::new(move || {
        MachineConnection::for_identity(&identity)
    }))
}

#[cfg(feature = "test-support")]
pub fn with_paired_presentation(identity: &str) -> Arc<PairedClients> {
    let presentation = paired_presentation(identity);
    presentation.prepare().expect("fixture account context");
    presentation
}
