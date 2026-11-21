//! Desktop presentation and retained state for the portable paired connection registry.
use spaceterm_client::direct::{DirectConnection, DirectConnector, DirectError, PairingId};
use spaceterm_core::MachineConnection;
use std::sync::Arc;

pub use spaceterm_client::{
    ConnectionStore, DirectRoute, DirectRouteId, DirectRouteStatus, EnrollmentStage, JoinId,
    JoinStatus, PendingUnpair, RefusalReason, StoreKind, UnavailableReason, UnpairProgress,
};
#[cfg(test)]
pub use spaceterm_client::{PendingEnrollment, SavedConnection, UnpairState};

#[derive(Clone)]
pub struct DirectConnections {
    routes: spaceterm_client::DirectConnections,
    identity: String,
}

impl DirectConnections {
    pub fn with_credentials(
        identity: &str,
        records: Arc<dyn spaceterm_client::records::Records>,
        credentials: Arc<dyn spaceterm_client::CredentialStore>,
    ) -> Self {
        Self {
            routes: spaceterm_core::native_client::paired_routes(identity, records, credentials),
            identity: identity.to_owned(),
        }
    }

    pub fn core(&self, id: &DirectRouteId) -> Option<MachineConnection> {
        MachineConnection::for_paired_client(&self.identity, self.routes.core(id)?).ok()
    }

    pub fn service(&self, id: &DirectRouteId) -> Option<MachineConnection> {
        MachineConnection::for_paired_client(&self.identity, self.routes.service(id)?).ok()
    }

    /// Looks up retained presentation state without opening a network connection.
    pub fn storage(&self, id: &DirectRouteId) -> Option<MachineConnection> {
        struct Offline;
        impl DirectConnector for Offline {
            fn connect(&self) -> Result<DirectConnection, DirectError> {
                Err(DirectError::Transport)
            }
        }

        let route = self.routes.route(id)?;
        MachineConnection::direct(&self.identity, route.id, route.paired, Arc::new(Offline)).ok()
    }

    pub fn revoke_other(
        &self,
        core: MachineConnection,
        target: PairingId,
        label: String,
    ) -> Result<(), DirectError> {
        let pin = core.direct_status()?.pin;
        if !core.is_account_local() {
            return Err(DirectError::AuthenticationRefused);
        }
        self.routes.revoke_other(pin, target, label)
    }
}

impl std::ops::Deref for DirectConnections {
    type Target = spaceterm_client::DirectConnections;

    fn deref(&self) -> &Self::Target {
        &self.routes
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use spaceterm_client::tests as fixture;
    use zeroize::Zeroizing;

    fn desktop(routes: spaceterm_client::DirectConnections) -> DirectConnections {
        DirectConnections {
            identity: "spaceterm-development".to_owned(),
            routes,
        }
    }

    pub fn offline(label: &str) -> (DirectConnections, DirectRouteId) {
        let (routes, id) = fixture::offline(label);
        (desktop(routes), id)
    }

    pub fn offline_with_lock(
        label: &str,
    ) -> (DirectConnections, DirectRouteId, fixture::CredentialLock) {
        let (routes, id, lock) = fixture::offline_with_lock(label);
        (desktop(routes), id, lock)
    }

    pub fn offline_with_storage(
        label: &str,
    ) -> (DirectConnections, DirectRouteId, fixture::RecordsStorage) {
        let (routes, id, storage) = fixture::offline_with_storage(label);
        (desktop(routes), id, storage)
    }

    pub fn connected_routes(labels: &[&str]) -> (DirectConnections, Vec<DirectRouteId>) {
        let (routes, ids) = fixture::connected_routes(labels);
        (desktop(routes), ids)
    }

    pub fn connected_routes_to_one_core(
        labels: &[&str],
        core: [u8; 32],
    ) -> (DirectConnections, Vec<DirectRouteId>) {
        let (routes, ids) = fixture::connected_routes_to_one_core(labels, core);
        (desktop(routes), ids)
    }

    pub fn disconnected_routes_at(
        records: &[(&str, &str)],
    ) -> (DirectConnections, Vec<DirectRouteId>) {
        let (routes, ids) = fixture::disconnected_routes_at(records);
        (desktop(routes), ids)
    }

    pub fn waiting_invitation(
        label: &str,
    ) -> (
        DirectConnections,
        Zeroizing<String>,
        spaceterm_client::direct::ComparisonCode,
    ) {
        let (routes, link, code) = fixture::waiting_invitation(label);
        (desktop(routes), link, code)
    }

    pub fn lost_claim_invitation(label: &str) -> (DirectConnections, Zeroizing<String>) {
        let (routes, link) = fixture::lost_claim_invitation(label);
        (desktop(routes), link)
    }

    pub fn uncertain_join(label: &str) -> (DirectConnections, JoinId, DirectRouteId) {
        let (routes, join, id) = fixture::uncertain_join(label);
        (desktop(routes), join, id)
    }

    pub fn retryable_join(label: &str, stage: EnrollmentStage) -> (DirectConnections, JoinId) {
        let (routes, join) = fixture::retryable_join(label, stage);
        (desktop(routes), join)
    }

    pub fn unconfirmed_join(label: &str) -> (DirectConnections, JoinId) {
        let (routes, join) = fixture::unconfirmed_join(label);
        (desktop(routes), join)
    }

    pub use fixture::saved;
}
