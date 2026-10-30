//! Restricted account authority for an exact retained direct-network revocation.

use spaceterm_server::ServerConnection;
use spaceterm_server::direct::DirectError;

use super::AccountRevocationResolver;
use super::credentials::Interaction;
use super::records::AccountRoute;
pub(super) struct LocalRevocationResolver(pub(super) ServerConnection);
impl AccountRevocationResolver for LocalRevocationResolver {
    fn route_for(&self, server: &ServerConnection) -> Result<AccountRoute, DirectError> {
        if server.is_account_local() {
            Ok(AccountRoute::Local)
        } else {
            Err(DirectError::AuthenticationRefused)
        }
    }
    fn resolve(
        &self,
        route: &AccountRoute,
        _: Interaction,
    ) -> Result<ServerConnection, DirectError> {
        match route {
            AccountRoute::Local => Ok(self.0.clone()),
        }
    }
}
