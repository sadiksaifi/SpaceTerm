//! Protocol connection integration with the actor's committed extension facts.

use crate::{
    api::ServerHandle,
    contracts::{ServerFailure, ServerFailureClass},
    extensions::ExtensionObserverPort,
    failure::failure,
    listeners::ServerListeners,
};
use spaceterm_core::{
    identity::ClientId,
    observations::ConfirmedDirectoryObservation,
    operation_ledger_contracts::CanonicalOperationRequest,
    pairing::{PairingState, PairingWrite, PairingWrites},
    storage::CommitReceipt,
};
use std::sync::Arc;

/// A synchronous cancellation notification. The retained connection supervisor
/// performs and joins native close; it does not release Sessions or their grants.
pub trait OrdinaryConnectionRevocationPort: Send + Sync {
    fn request_close(&self, clients: &[ClientId]) -> Result<(), ServerFailure>;
}
impl OrdinaryConnectionRevocationPort for ServerListeners {
    fn request_close(&self, clients: &[ClientId]) -> Result<(), ServerFailure> {
        self.request_close_clients(clients)
    }
}

/// Install this decorator in ServerConfig.extensions. Pairing's lookup projection
/// installs first, then every exact retained Client receives cancellation. A new
/// pairing cannot be closed by the old lifetime's notification.
pub struct PairingConnectionObserver {
    inner: Arc<dyn ExtensionObserverPort>,
    connections: Arc<dyn OrdinaryConnectionRevocationPort>,
}
impl PairingConnectionObserver {
    pub fn new(
        inner: Arc<dyn ExtensionObserverPort>,
        connections: Arc<dyn OrdinaryConnectionRevocationPort>,
    ) -> Self {
        Self { inner, connections }
    }
}
impl ExtensionObserverPort for PairingConnectionObserver {
    fn admitted(
        &self,
        request: CanonicalOperationRequest,
        receipt: CommitReceipt,
        handle: ServerHandle,
    ) -> Result<(), ServerFailure> {
        self.inner.admitted(request, receipt, handle)
    }
    fn pairing_committed(
        &self,
        writes: &PairingWrites,
        receipt: CommitReceipt,
    ) -> Result<(), ServerFailure> {
        if receipt.store_revision.get() == 0 {
            return Err(failure(ServerFailureClass::RecoveryRequired));
        }
        self.inner.pairing_committed(writes, receipt)?;
        for write in &writes.parts().writes {
            if let PairingWrite::PutAuthorization { value, .. } = write
                && let PairingState::Revoked { revision } = value.parts().record.state
            {
                if revision > receipt.store_revision {
                    return Err(failure(ServerFailureClass::RecoveryRequired));
                }
                // Each authorization is independently bounded. Borrowed chunks
                // avoid allocating an unbounded aggregate of retained lifetimes.
                for client in &value.parts().clients {
                    self.connections
                        .request_close(&[client.parts().client.id])?;
                }
            }
        }
        Ok(())
    }
    fn confirmed_directory_committed(
        &self,
        observation: &ConfirmedDirectoryObservation,
        receipt: CommitReceipt,
    ) {
        self.inner
            .confirmed_directory_committed(observation, receipt);
    }
}
