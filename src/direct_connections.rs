//! Desktop presentation and retained state for the portable paired connection registry.
use spaceterm_client::direct::{DirectError, Endpoint, PairingId};
use spaceterm_client::pane::control::ClientName;
use spaceterm_core::{CoreError, MachineConnection, native_client::PairedClients};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use zeroize::Zeroizing;

pub use spaceterm_client::{
    ConnectionStore, DirectRoute, DirectRouteId, DirectRouteStatus, EnrollmentStage, JoinId,
    JoinStatus, PendingUnpair, RefusalReason, StoreKind, UnavailableReason, UnpairProgress,
};
#[cfg(test)]
pub use spaceterm_client::{PendingEnrollment, SavedConnection, UnpairState};

#[derive(Clone)]
pub struct DirectConnections(Arc<DesktopRoutes>);

struct DesktopRoutes {
    routes: spaceterm_client::DirectConnections,
    presentation: Arc<PairedClients>,
    starting: AtomicBool,
}

impl DesktopRoutes {
    fn prepare(&self) -> Result<(), CoreError> {
        self.presentation.prepare()?;
        self.routes.resume_retained();
        Ok(())
    }
}

impl DirectConnections {
    pub fn with_credentials(
        identity: &str,
        records: Arc<dyn spaceterm_client::records::Records>,
        credentials: Arc<dyn spaceterm_client::CredentialStore>,
    ) -> Self {
        let (routes, presentation) =
            spaceterm_core::native_client::paired_routes(identity, records, credentials);
        Self(Arc::new(DesktopRoutes {
            routes,
            presentation,
            starting: AtomicBool::new(false),
        }))
    }

    /// Resolves native paths and loads the retained registry off the UI thread. A route can
    /// become Connected only after its native context is available.
    pub fn resume_retained(&self) {
        if self.0.starting.swap(true, Ordering::AcqRel) {
            return;
        }
        let weak = Arc::downgrade(&self.0);
        let started = std::thread::Builder::new()
            .name("spaceterm-paired-startup".into())
            .spawn(move || {
                while let Some(owner) = weak.upgrade() {
                    if owner.prepare().is_ok() {
                        return;
                    }
                    drop(owner);
                    std::thread::sleep(std::time::Duration::from_millis(250));
                }
            });
        if started.is_err() {
            self.0.starting.store(false, Ordering::Release);
        }
    }

    /// Runs on the enrollment worker. The native account and retained registry must be ready
    /// before an Invitation Link can publish a new connected route.
    pub fn join(
        &self,
        text: Zeroizing<String>,
        endpoint: Option<Endpoint>,
        name: ClientName,
    ) -> Result<JoinId, DirectError> {
        self.0.prepare()?;
        if !self.0.routes.retained_inventory_loaded() {
            return Err(DirectError::StorageUnavailable);
        }
        self.0.routes.join(text, endpoint, name)
    }

    pub fn core(&self, id: &DirectRouteId) -> Option<MachineConnection> {
        self.0.presentation.bind(self.0.routes.core(id)?).ok()
    }

    pub fn service(&self, id: &DirectRouteId) -> Option<MachineConnection> {
        self.0.presentation.bind(self.0.routes.service(id)?).ok()
    }

    /// Looks up retained presentation state without opening a network connection.
    pub fn storage(&self, id: &DirectRouteId) -> Option<MachineConnection> {
        let route = self.0.routes.route(id)?;
        self.0.presentation.offline(route.id, route.paired).ok()
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
        self.0.routes.revoke_other(pin, target, label)
    }
}

impl std::ops::Deref for DirectConnections {
    type Target = spaceterm_client::DirectConnections;

    fn deref(&self) -> &Self::Target {
        &self.0.routes
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use spaceterm_client::tests as fixture;

    fn desktop(routes: spaceterm_client::DirectConnections) -> DirectConnections {
        DirectConnections(Arc::new(DesktopRoutes {
            routes,
            presentation: spaceterm_core::native_client::with_paired_presentation(
                "spaceterm-development",
            ),
            starting: AtomicBool::new(true),
        }))
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
