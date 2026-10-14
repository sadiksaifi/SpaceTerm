//! Read-only desktop discovery of the retained account/build server.
//! Launch never creates a server identity, requests authorization, starts a deliberately
//! stopped service, activates staged code, or enrolls a replacement client binding.

pub(crate) mod enrollment;
pub(crate) mod foreground;
mod setup;
pub(crate) use setup::offer as setup_offer;

use spaceterm_client::{
    api::PairedServerBinding,
    failure::{ClientFailure, ClientFailureClass},
    journal::JournalController,
};
use spaceterm_client_native::{NativeClientProvider, route::LocalRouteAuthority};
use spaceterm_host::{
    account_service::{RegisteredServer, ServiceFailure},
    startup::DesiredServerState,
};

/// Registration authority is retained even when no ordinary desktop binding is available.
/// An absent binding requires an explicit foreground Add Server flow, not automatic enrollment.
#[allow(
    dead_code,
    reason = "native provider consumes retained local discovery at startup"
)]
pub(crate) struct LocalServer {
    pub(crate) registration: RegisteredServer,
    pub(crate) binding: Option<PairedServerBinding>,
    pub(crate) route: Option<LocalRouteAuthority>,
    pub(crate) route_failure: Option<ClientFailure>,
}

#[allow(
    dead_code,
    reason = "native provider supplies the one recovered client journal"
)]
pub(crate) fn discover(
    provider: &NativeClientProvider,
    journal: &JournalController,
) -> Result<Option<LocalServer>, ClientFailure> {
    let Some(registration) = provider
        .management()
        .supervisor()
        .discover()
        .map_err(classify)?
    else {
        return Ok(None);
    };
    let pin = registration.server_pin();
    let snapshot = journal.snapshot()?;
    let saved = snapshot
        .bindings()
        .iter()
        .filter_map(|saved| saved.binding().ok())
        .find(|binding| binding.target().server().bytes() == pin.identity);
    let binding = match saved {
        Some(binding) if binding.target().pin() != pin => {
            return Err(ClientFailure::new(ClientFailureClass::PinMismatch));
        }
        Some(binding) if journal.ordinary_use_allowed(&binding)? => Some(binding),
        _ => None,
    };
    // Deliberate Stop and ordinary-use suppression are checked before opening the private
    // endpoint. A missing endpoint is an offline route, never permission to install or Start.
    let (route, route_failure) = if binding.is_some()
        && registration.is_installed()
        && registration.desired_state() == DesiredServerState::Running
    {
        match registration.endpoint() {
            Ok(endpoint) => match LocalRouteAuthority::new(endpoint) {
                Ok(route) => (Some(route), None),
                Err(error) => (None, Some(error)),
            },
            Err(ServiceFailure::Missing) => (
                None,
                Some(ClientFailure::new(ClientFailureClass::Unavailable)),
            ),
            Err(error) => (None, Some(classify(error))),
        }
    } else {
        (None, None)
    };
    if let (Some(binding), Some(route)) = (&binding, &route)
        && binding.target().route() != route.description()
    {
        return Err(ClientFailure::new(ClientFailureClass::StaleAuthority));
    }
    Ok(Some(LocalServer {
        registration,
        binding,
        route,
        route_failure,
    }))
}

/// Shares native Stop intent with the connection actor before it reads a credential or dials.
#[allow(
    dead_code,
    reason = "native connection composition injects trusted local discovery"
)]
pub(crate) fn trusted_discovery(
    provider: NativeClientProvider,
) -> std::sync::Arc<dyn spaceterm_client::ports::TrustedDiscovery> {
    std::sync::Arc::new(LocalDiscovery(provider))
}

struct LocalDiscovery(NativeClientProvider);
impl spaceterm_client::ports::TrustedDiscovery for LocalDiscovery {
    fn status<'a>(
        &'a self,
        target: &'a spaceterm_client::api::ConnectionTarget,
    ) -> spaceterm_client::ports::ClientFuture<'a, spaceterm_client::ports::DiscoveryState> {
        // Native registration belongs only to This Computer. Remote connections must not
        // depend on this account's local service storage or acquire local endpoint authority.
        if !matches!(
            target.route().parts(),
            spaceterm_client::api::RouteParts::Ipc { .. }
        ) {
            return Box::pin(async { Ok(spaceterm_client::ports::DiscoveryState::Unverified) });
        }
        let provider = self.0.clone();
        let pin = target.pin();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                use spaceterm_client::ports::DiscoveryState;
                let Some(registration) = provider
                    .management()
                    .supervisor()
                    .discover()
                    .map_err(classify)?
                else {
                    return Ok(DiscoveryState::Unverified);
                };
                if registration.server_pin().identity != pin.identity {
                    return Ok(DiscoveryState::Unverified);
                }
                if registration.server_pin() != pin {
                    return Err(ClientFailure::new(ClientFailureClass::PinMismatch));
                }
                Ok(match registration.desired_state() {
                    DesiredServerState::Stopped => DiscoveryState::Stopped,
                    DesiredServerState::Running => DiscoveryState::Running,
                })
            })
            .await
            .map_err(|_| ClientFailure::new(ClientFailureClass::Unavailable))?
        })
    }
}

pub(crate) fn classify(failure: ServiceFailure) -> ClientFailure {
    let class = match failure {
        ServiceFailure::StorageLocked
        | ServiceFailure::StorageUnavailable
        | ServiceFailure::Missing => ClientFailureClass::StorageUnavailable,
        ServiceFailure::UnsupportedSchema => ClientFailureClass::UnknownSchema,
        ServiceFailure::IncompatibleStorage => ClientFailureClass::Incompatible,
        ServiceFailure::UnsafeStorage => ClientFailureClass::UnsafeStorage,
        ServiceFailure::InvalidRecord => ClientFailureClass::InvalidStorage,
        ServiceFailure::IdentityMismatch => ClientFailureClass::PinMismatch,
        ServiceFailure::LimitExceeded => ClientFailureClass::ResourceLimit,
        ServiceFailure::Indeterminate => ClientFailureClass::RecoveryRequired,
        ServiceFailure::RootAccount | ServiceFailure::AuthorizationDenied => {
            ClientFailureClass::AuthorizationDenied
        }
        _ => ClientFailureClass::Unavailable,
    };
    ClientFailure::new(class)
}

#[cfg(test)]
mod paired_tests;
#[cfg(test)]
pub(in crate::platform) mod tests;
