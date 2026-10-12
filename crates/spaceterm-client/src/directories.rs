//! Directory chooser reads on the owning server. Returned values deliberately
//! expose no Path, native handle, or client-local filesystem capability.

use crate::git::rpc::{GitClientFuture, GitConnectionState, GitReadiness};
use spaceterm_core::{identity::ServerId, organization::ServerDirectory, server_directories::*};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ClientDirectoryFailure {
    #[error("server is offline")]
    Offline,
    #[error("server state is not reconciled")]
    Reconciling,
    #[error("directory target belongs to another server")]
    WrongServer,
    #[error("stale directory response")]
    StaleResponse,
    #[error("directory connection provider unavailable")]
    Unavailable,
    #[error("invalid directory response")]
    InvalidResponse,
    #[error("directory request rejected ({0})")]
    Remote(DirectoryFailure),
}
/// The existing client-core worker supplies authenticated context, query
/// correlation and cancellation. This read facade never opens a transport.
pub trait DirectoryConnectionPort: Send + Sync {
    fn state(&self) -> Result<GitConnectionState, ClientDirectoryFailure>;
    fn query(
        &self,
        query: DirectoryQuery,
    ) -> GitClientFuture<'_, Result<DirectoryQueryResult, ClientDirectoryFailure>>;
}
#[derive(Clone)]
pub struct DirectoryClient {
    server: ServerId,
    connection: Arc<dyn DirectoryConnectionPort>,
}
impl DirectoryClient {
    pub fn new(
        server: ServerId,
        connection: Arc<dyn DirectoryConnectionPort>,
    ) -> Result<Self, ClientDirectoryFailure> {
        if connection.state()?.context.server() != server {
            return Err(ClientDirectoryFailure::WrongServer);
        }
        Ok(Self { server, connection })
    }
    pub fn server(&self) -> ServerId {
        self.server
    }
    pub async fn home(&self) -> Result<ServerDirectory, ClientDirectoryFailure> {
        let DirectoryQueryResult::Home(home) = self.query(DirectoryQueryAction::Home).await? else {
            return Err(ClientDirectoryFailure::InvalidResponse);
        };
        Ok(home)
    }
    pub async fn browse(
        &self,
        directory: ServerDirectory,
        cursor: Option<DirectoryCursor>,
    ) -> Result<DirectoryPage, ClientDirectoryFailure> {
        let DirectoryQueryResult::Page(page) = self
            .query(DirectoryQueryAction::Browse { directory, cursor })
            .await?
        else {
            return Err(ClientDirectoryFailure::InvalidResponse);
        };
        Ok(*page)
    }
    pub async fn validate(
        &self,
        directory: ServerDirectory,
        purpose: DirectoryPurpose,
    ) -> Result<DirectoryValidation, ClientDirectoryFailure> {
        let DirectoryQueryResult::Validation(validation) = self
            .query(DirectoryQueryAction::Validate { directory, purpose })
            .await?
        else {
            return Err(ClientDirectoryFailure::InvalidResponse);
        };
        Ok(validation)
    }
    pub async fn query(
        &self,
        action: DirectoryQueryAction,
    ) -> Result<DirectoryQueryResult, ClientDirectoryFailure> {
        let state = self.connection.state()?;
        if state.context.server() != self.server {
            return Err(ClientDirectoryFailure::WrongServer);
        }
        if state.context.parts().epoch.get() == 0 || state.generation.get() == 0 {
            return Err(ClientDirectoryFailure::InvalidResponse);
        }
        match state.readiness {
            GitReadiness::Ready => {}
            GitReadiness::Offline => return Err(ClientDirectoryFailure::Offline),
            GitReadiness::Reconciling => return Err(ClientDirectoryFailure::Reconciling),
        }
        let query = DirectoryQuery::new(DirectoryQueryParts {
            context: state.context,
            action,
        })
        .map_err(ClientDirectoryFailure::Remote)?;
        let result = self.connection.query(query.clone()).await?;
        if self.connection.state()? != state {
            return Err(ClientDirectoryFailure::StaleResponse);
        }
        match (&query.parts().action, &result) {
            (DirectoryQueryAction::Home, DirectoryQueryResult::Home(home))
                if home.server() == self.server => {}
            (
                DirectoryQueryAction::Browse { directory, cursor },
                DirectoryQueryResult::Page(page),
            ) => {
                let p = page.parts();
                if p.context != state.context || &p.directory != directory {
                    return Err(ClientDirectoryFailure::StaleResponse);
                }
                if cursor.as_ref().is_some_and(|c| {
                    c.parts().observation != p.observation || p.next.as_ref() == Some(c)
                }) {
                    return Err(ClientDirectoryFailure::Remote(
                        DirectoryFailure::StaleCursor,
                    ));
                }
            }
            (
                DirectoryQueryAction::Validate { directory, purpose },
                DirectoryQueryResult::Validation(v),
            ) if &v.directory == directory && &v.purpose == purpose => {}
            _ => return Err(ClientDirectoryFailure::InvalidResponse),
        }
        Ok(result)
    }
}
