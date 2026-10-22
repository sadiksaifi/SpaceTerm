//! A scripted machine for Directory Picker tests, standing in for another computer's server.
use std::sync::Arc;

use gpui::Task;

use super::{
    DirectoryListing, DirectorySelection, DirectorySource, DirectorySourceError, ExactPathState,
    PickerPath, SourceFuture,
};
use crate::domain::RemoteDirectory;

/// Each scripted answer fails as a source would.
pub(super) type RemoteDirectoryProviderError = DirectorySourceError;

/// The scripted machine's answers. Every path crossing it is the other machine's string.
pub(super) trait RemoteDirectoryProvider: Send + Sync {
    /// The home directory's absolute path.
    fn discover_account(&self) -> Task<Result<String, RemoteDirectoryProviderError>>;

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
    ) -> Task<Result<String, RemoteDirectoryProviderError>>;
}

/// Reads the scripted machine named `host`. A selection is that machine's validated path, as a
/// server's is.
pub(super) struct RemoteDirectorySource {
    provider: Arc<dyn RemoteDirectoryProvider + Send + Sync>,
    host: String,
}

impl RemoteDirectorySource {
    pub(super) fn new(
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
        Box::pin(
            async move { PickerPath::new(account.await?).map_err(|_| DirectorySourceError::Other) },
        )
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

    fn select(&self, directory: PickerPath) -> SourceFuture<DirectorySelection> {
        let directory = match remote_directory(directory) {
            Ok(directory) => directory,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        let identity = self.provider.validate_physical_identity(directory);
        Box::pin(async move { Ok(DirectorySelection::Server(identity.await?.into())) })
    }
}

fn remote_directory(directory: PickerPath) -> Result<RemoteDirectory, DirectorySourceError> {
    RemoteDirectory::new(directory.into_string()).map_err(|_| DirectorySourceError::Other)
}

fn resolve<T: 'static>(task: Task<Result<T, RemoteDirectoryProviderError>>) -> SourceFuture<T> {
    Box::pin(task)
}
