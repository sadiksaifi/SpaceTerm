//! Desktop injection of the shared headless native-client route provider.

pub(crate) mod askpass;

use spaceterm_client::{
    api::{ConnectionTarget, RouteParts},
    failure::{ClientFailure, ClientFailureClass},
    journal::JournalController,
    ports::{ClientFuture, ConnectedRoute, ConnectionPurpose, RouteConnector},
};
use spaceterm_client_native::route::{
    LocalRouteAuthority, NativeRouteConnector, SshRouteConfiguration,
};
use std::sync::Arc;

/// C3 supplies native-discovery endpoint authority and its existing per-process
/// runtime/AskPass facilities. Saved route strings are not local file authority.
pub(crate) fn connector(
    local: Vec<LocalRouteAuthority>,
    ssh: Option<SshRouteConfiguration>,
) -> Result<Arc<dyn RouteConnector>, ClientFailure> {
    Ok(Arc::new(NativeRouteConnector::new(local, ssh)?))
}

/// Captured local process configuration, never recovered from a remote route description.
#[derive(Clone)]
pub(crate) struct SshStartup {
    pub(crate) executable: std::path::PathBuf,
    pub(crate) home: std::path::PathBuf,
    pub(crate) environment: Vec<(std::ffi::OsString, std::ffi::OsString)>,
}
pub(crate) fn ssh_configuration(
    paths: &super::app_paths::AppPaths,
    startup: &SshStartup,
    askpass: Option<Arc<dyn spaceterm_host::ssh_tunnel::TunnelAskPass>>,
) -> Result<(SshRouteConfiguration, super::app_paths::RuntimeOwner), ClientFailure> {
    use spaceterm_client_native::route::TunnelEndpoints;
    crate::ssh::managed_hosts::ManagedHostsStore::new(paths)
        .ensure_exists()
        .map_err(|_| ClientFailure::new(ClientFailureClass::StorageUnavailable))?;
    let owner = paths
        .create_runtime_owner("t")
        .map_err(|_| ClientFailure::new(ClientFailureClass::UnsafeEndpoint))?;
    let socket = owner
        .socket_path("t")
        .map_err(|_| ClientFailure::new(ClientFailureClass::UnsafeEndpoint))?;
    let directory = spaceterm_host::ipc::PrivateIpcDirectory::open(
        socket
            .parent()
            .ok_or_else(|| ClientFailure::new(ClientFailureClass::UnsafeEndpoint))?,
    )
    .map_err(|_| ClientFailure::new(ClientFailureClass::UnsafeEndpoint))?;
    let endpoints = TunnelEndpoints::new(directory)?;
    // Check the longest allocator name now, before any route is dialed.
    let _ = endpoints.allocate()?;
    Ok((
        SshRouteConfiguration::new(
            startup.executable.clone(),
            paths.managed_ssh_config(),
            startup.home.clone(),
            startup.environment.clone(),
            endpoints,
            askpass,
        )?,
        owner,
    ))
}

/// Keep the app-owned tunnel directory alive for every connection owned by this connector.
struct RuntimeRoutes {
    routes: Arc<dyn RouteConnector>,
    _owner: super::app_paths::RuntimeOwner,
}
pub(crate) fn retain_runtime(
    routes: Arc<dyn RouteConnector>,
    owner: super::app_paths::RuntimeOwner,
) -> Arc<dyn RouteConnector> {
    Arc::new(RuntimeRoutes {
        routes,
        _owner: owner,
    })
}
impl RouteConnector for RuntimeRoutes {
    fn open<'a>(
        &'a self,
        target: &'a ConnectionTarget,
        purpose: ConnectionPurpose,
    ) -> ClientFuture<'a, ConnectedRoute> {
        self.routes.open(target, purpose)
    }
}

/// IPC authority is rediscovered from the native registration on every attempt. A saved
/// pathname cannot become authority, and a stopped launch does not freeze an empty list.
#[cfg(test)]
pub(crate) fn refreshing_connector(
    native: Option<spaceterm_client_native::NativeClientProvider>,
    journal: Arc<JournalController>,
    remote: Arc<dyn RouteConnector>,
) -> Arc<dyn RouteConnector> {
    refreshing_connector_with_failure(native, None, journal, remote)
}
pub(crate) fn refreshing_connector_with_failure(
    native: Option<spaceterm_client_native::NativeClientProvider>,
    failure: Option<ClientFailure>,
    journal: Arc<JournalController>,
    remote: Arc<dyn RouteConnector>,
) -> Arc<dyn RouteConnector> {
    Arc::new(RefreshingRoutes {
        native,
        failure,
        journal,
        remote,
    })
}
struct RefreshingRoutes {
    failure: Option<ClientFailure>,
    native: Option<spaceterm_client_native::NativeClientProvider>,
    journal: Arc<JournalController>,
    remote: Arc<dyn RouteConnector>,
}
impl RouteConnector for RefreshingRoutes {
    fn open<'a>(
        &'a self,
        target: &'a ConnectionTarget,
        purpose: ConnectionPurpose,
    ) -> ClientFuture<'a, ConnectedRoute> {
        Box::pin(async move {
            if purpose != ConnectionPurpose::Ordinary {
                return Err(ClientFailure::new(ClientFailureClass::AuthorizationDenied));
            }
            if !matches!(target.route().parts(), RouteParts::Ipc { .. }) {
                return self.remote.open(target, purpose).await;
            }
            let native = self.native.clone().ok_or_else(|| {
                self.failure
                    .unwrap_or_else(|| ClientFailure::new(ClientFailureClass::Unavailable))
            })?;
            let journal = self.journal.clone();
            let local = tokio::task::spawn_blocking(move || {
                super::local_server_client::discover(&native, &journal)
            })
            .await
            .map_err(|_| ClientFailure::new(ClientFailureClass::Unavailable))??
            .ok_or_else(|| ClientFailure::new(ClientFailureClass::Unavailable))?;
            if local.registration.server_pin() != target.pin()
                || local
                    .binding
                    .as_ref()
                    .is_none_or(|binding| binding.target() != target)
            {
                return Err(ClientFailure::new(ClientFailureClass::StaleAuthority));
            }
            let authority = local.route.ok_or_else(|| {
                local
                    .route_failure
                    .unwrap_or_else(|| ClientFailure::new(ClientFailureClass::Unavailable))
            })?;
            NativeRouteConnector::new(vec![authority], None)?
                .open(target, purpose)
                .await
        })
    }
}

#[cfg(test)]
mod tests;

pub(crate) fn failed_local_discovery(
    failure: ClientFailure,
) -> Arc<dyn spaceterm_client::ports::TrustedDiscovery> {
    Arc::new(FailedLocalDiscovery(failure))
}
struct FailedLocalDiscovery(ClientFailure);
impl spaceterm_client::ports::TrustedDiscovery for FailedLocalDiscovery {
    fn status<'a>(
        &'a self,
        target: &'a ConnectionTarget,
    ) -> ClientFuture<'a, spaceterm_client::ports::DiscoveryState> {
        Box::pin(async move {
            if matches!(target.route().parts(), RouteParts::Ipc { .. }) {
                Err(self.0)
            } else {
                Ok(spaceterm_client::ports::DiscoveryState::Unverified)
            }
        })
    }
}
