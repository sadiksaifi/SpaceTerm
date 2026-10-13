//! Desktop injection of the shared headless native-client route provider.

use spaceterm_client::{failure::ClientFailure, ports::RouteConnector};
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

#[cfg(test)]
mod tests;
