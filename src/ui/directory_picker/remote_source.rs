//! The Directory Picker's source for a connected SSH Destination.
use std::sync::Arc;

use gpui::Task;

use super::{
    DirectoryListing, DirectorySource, DirectorySourceError, ExactPathState, PickerPath,
    SourceFuture,
};
use crate::domain::{PinnedDirectory, RemoteDirectory, RemoteDirectoryIdentity};
use crate::ssh::remote_account::RemoteWorkspaceAccount;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RemoteDirectoryProviderError {
    ConnectionLost,
    /// The server refused a session on a Control Connection that is still live.
    SessionUnavailable,
    Missing,
    NotDirectory,
    PermissionDenied,
    UnsupportedLoginShell,
    InvalidResponse,
    Other,
}

/// The connected-SSH boundary. Every path crossing it is a remote string type.
pub(crate) trait RemoteDirectoryProvider: Send + Sync {
    fn discover_account(
        &self,
    ) -> Task<Result<RemoteWorkspaceAccount, RemoteDirectoryProviderError>>;

    fn list_directories(
        &self,
        directory: RemoteDirectory,
    ) -> Task<Result<DirectoryListing, RemoteDirectoryProviderError>>;

    fn probe_exact_path(
        &self,
        directory: RemoteDirectory,
    ) -> Task<Result<ExactPathState, RemoteDirectoryProviderError>>;

    fn create_directory_recursively(
        &self,
        directory: RemoteDirectory,
    ) -> Task<Result<(), RemoteDirectoryProviderError>>;

    fn validate_physical_identity(
        &self,
        directory: RemoteDirectory,
    ) -> Task<Result<RemoteDirectoryIdentity, RemoteDirectoryProviderError>>;
}

/// Reads one SSH Destination's directories through its connected provider.
pub(crate) struct RemoteDirectorySource {
    provider: Arc<dyn RemoteDirectoryProvider + Send + Sync>,
    host: String,
}

impl RemoteDirectorySource {
    /// Creates a source for the machine `host` names.
    pub(crate) fn new(
        provider: Arc<dyn RemoteDirectoryProvider + Send + Sync>,
        host: &str,
    ) -> Self {
        Self {
            provider,
            host: host.to_owned(),
        }
    }
}

impl DirectorySource for RemoteDirectorySource {
    fn machine_name(&self) -> Option<&str> {
        Some(&self.host)
    }

    fn discover_home(&self) -> SourceFuture<PickerPath> {
        let account = self.provider.discover_account();
        Box::pin(async move {
            let account = account.await.map_err(source_error)?;
            PickerPath::new(account.home_identity().as_str().to_owned())
                .map_err(|_| DirectorySourceError::Other)
        })
    }

    fn list_directories(&self, directory: PickerPath) -> SourceFuture<DirectoryListing> {
        match remote_directory(directory) {
            Ok(directory) => resolve(self.provider.list_directories(directory)),
            Err(error) => Box::pin(async move { Err(error) }),
        }
    }

    fn probe_exact_path(&self, directory: PickerPath) -> SourceFuture<ExactPathState> {
        match remote_directory(directory) {
            Ok(directory) => resolve(self.provider.probe_exact_path(directory)),
            Err(error) => Box::pin(async move { Err(error) }),
        }
    }

    fn create_directory_recursively(&self, directory: PickerPath) -> SourceFuture<()> {
        match remote_directory(directory) {
            Ok(directory) => resolve(self.provider.create_directory_recursively(directory)),
            Err(error) => Box::pin(async move { Err(error) }),
        }
    }

    fn pin(&self, directory: PickerPath) -> SourceFuture<PinnedDirectory> {
        let directory = match remote_directory(directory) {
            Ok(directory) => directory,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        let identity = self.provider.validate_physical_identity(directory.clone());
        Box::pin(async move {
            let identity = identity.await.map_err(source_error)?;
            Ok(PinnedDirectory::Remote {
                directory,
                identity,
            })
        })
    }
}

fn remote_directory(directory: PickerPath) -> Result<RemoteDirectory, DirectorySourceError> {
    RemoteDirectory::new(directory.into_string()).map_err(|_| DirectorySourceError::Other)
}

fn resolve<T: 'static>(task: Task<Result<T, RemoteDirectoryProviderError>>) -> SourceFuture<T> {
    Box::pin(async move { task.await.map_err(source_error) })
}

const fn source_error(error: RemoteDirectoryProviderError) -> DirectorySourceError {
    match error {
        RemoteDirectoryProviderError::ConnectionLost => DirectorySourceError::ConnectionLost,
        RemoteDirectoryProviderError::SessionUnavailable => {
            DirectorySourceError::SessionUnavailable
        }
        RemoteDirectoryProviderError::Missing => DirectorySourceError::Missing,
        RemoteDirectoryProviderError::NotDirectory => DirectorySourceError::NotDirectory,
        RemoteDirectoryProviderError::PermissionDenied => DirectorySourceError::PermissionDenied,
        RemoteDirectoryProviderError::UnsupportedLoginShell => {
            DirectorySourceError::UnsupportedLoginShell
        }
        RemoteDirectoryProviderError::InvalidResponse | RemoteDirectoryProviderError::Other => {
            DirectorySourceError::Other
        }
    }
}
