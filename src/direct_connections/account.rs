//! Restricted account authority for an exact retained direct-network revocation.

use std::sync::Arc;

use spaceterm_server::LocalServer;
use spaceterm_server::direct::DirectError;

use super::AccountRevocationResolver;
use super::credentials::Interaction;
use super::records::AccountRoute;
use crate::domain::SshDestination;
use crate::ssh::process::SshProcessAdapter;
use crate::ssh::server_connection::{ConnectMode, SshConnectFailure, SshServerConnections};
use crate::ssh::server_process::NativeSshConnectBackend;

pub(super) struct LocalRevocationResolver(pub(super) LocalServer);
impl AccountRevocationResolver for LocalRevocationResolver {
    fn route_for(&self, server: &LocalServer) -> Result<AccountRoute, DirectError> {
        if server.is_account_local() {
            Ok(AccountRoute::Local)
        } else {
            Err(DirectError::AuthenticationRefused)
        }
    }
    fn resolve(&self, route: &AccountRoute, _: Interaction) -> Result<LocalServer, DirectError> {
        match route {
            AccountRoute::Local => Ok(self.0.clone()),
            AccountRoute::Ssh { .. } => Err(DirectError::InteractionRequired),
        }
    }
}

pub(crate) struct NativeAccountRevocationResolver<A: SshProcessAdapter> {
    identity: String,
    local: LocalServer,
    connections: SshServerConnections,
    backend: Arc<NativeSshConnectBackend<A>>,
}

impl<A: SshProcessAdapter> NativeAccountRevocationResolver<A> {
    /// Uses the application's existing local handle and shared SSH backend. Construction
    /// performs no storage access, discovery, authentication or connection restoration.
    pub(crate) fn new(
        identity: &str,
        local: LocalServer,
        connections: SshServerConnections,
        backend: Arc<NativeSshConnectBackend<A>>,
    ) -> Self {
        Self {
            identity: identity.to_owned(),
            local,
            connections,
            backend,
        }
    }
}

impl<A: SshProcessAdapter> AccountRevocationResolver for NativeAccountRevocationResolver<A> {
    fn route_for(&self, server: &LocalServer) -> Result<AccountRoute, DirectError> {
        if server.is_account_local() && self.local.is_account_local() {
            return Ok(AccountRoute::Local);
        }
        let (destination, continuity) = self
            .connections
            .admitted_route_for(server)
            .ok_or(DirectError::AuthenticationRefused)?;
        if continuity.build != self.identity {
            return Err(DirectError::AuthenticationRefused);
        }
        Ok(AccountRoute::Ssh {
            destination: destination.as_str().to_owned(),
            continuity,
        })
    }

    fn resolve(
        &self,
        route: &AccountRoute,
        interaction: Interaction,
    ) -> Result<LocalServer, DirectError> {
        match route {
            AccountRoute::Local if self.local.is_account_local() => Ok(self.local.clone()),
            AccountRoute::Local => Err(DirectError::AuthenticationRefused),
            AccountRoute::Ssh {
                destination,
                continuity,
            } => {
                if continuity.build != self.identity {
                    return Err(DirectError::AuthenticationRefused);
                }
                let destination = SshDestination::new(destination.clone())
                    .map_err(|_| DirectError::AuthenticationRefused)?;
                self.backend
                    .resolve_account(
                        &destination,
                        continuity,
                        match interaction {
                            Interaction::Quiet => ConnectMode::QuietContinuation,
                            Interaction::Requested => ConnectMode::Requested,
                        },
                    )
                    .map_err(classify)
            }
        }
    }
}

fn classify(error: SshConnectFailure) -> DirectError {
    match error {
        SshConnectFailure::NeedsPerson(_) => DirectError::InteractionRequired,
        SshConnectFailure::Changed(_) | SshConnectFailure::Refused(_) => {
            DirectError::AuthenticationRefused
        }
        SshConnectFailure::Incompatible(_) => DirectError::IncompatibleProtocol,
        SshConnectFailure::Cancelled
        | SshConnectFailure::Stopped(_)
        | SshConnectFailure::Unavailable(_)
        | SshConnectFailure::Unconfirmed(_)
        | SshConnectFailure::InstallationOffered(_) => DirectError::Transport,
    }
}
