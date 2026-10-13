//! The existing picker over authenticated owning-server reads. A selected spelling remains a
//! ServerDirectory. It never becomes a PinnedDirectory or client-local filesystem authority.

use super::{
    DirectoryListing, DirectoryRow, DirectorySelection, DirectorySource, DirectorySourceError,
    ExactPathState, MAXIMUM_DIRECTORY_ROWS, PickerPath, SourceFuture,
};
use crate::domain::PinnedDirectory;
use spaceterm_client::directories::{ClientDirectoryFailure, DirectoryClient};
use spaceterm_core::{organization::ServerDirectory, server_directories::*};

pub(crate) struct ServerDirectorySource {
    client: DirectoryClient,
    machine: String,
    executor: tokio::runtime::Handle,
}

impl ServerDirectorySource {
    pub(crate) fn new(
        client: DirectoryClient,
        machine: String,
        executor: tokio::runtime::Handle,
    ) -> Self {
        Self {
            client,
            machine,
            executor,
        }
    }

    fn run<
        T: Send + 'static,
        F: std::future::Future<Output = Result<T, DirectorySourceError>> + Send + 'static,
    >(
        &self,
        operation: impl FnOnce(DirectoryClient) -> F,
    ) -> SourceFuture<T> {
        let task = self.executor.spawn(operation(self.client.clone()));
        Box::pin(async move {
            task.await
                .map_err(|_| DirectorySourceError::ConnectionLost)?
        })
    }
}

impl DirectorySource for ServerDirectorySource {
    fn machine_name(&self) -> Option<&str> {
        Some(&self.machine)
    }
    fn can_create(&self) -> bool {
        false
    }

    fn discover_home(&self) -> SourceFuture<PickerPath> {
        self.run(|client| async move {
            let home = client.home().await.map_err(source_error)?;
            let spelling =
                std::str::from_utf8(home.bytes()).map_err(|_| DirectorySourceError::Other)?;
            PickerPath::new(spelling.to_owned()).map_err(|_| DirectorySourceError::Other)
        })
    }

    fn list_directories(&self, directory: PickerPath) -> SourceFuture<DirectoryListing> {
        self.run(|client| async move {
            let directory = resolve(&client, directory).await?;
            let mut cursor = None;
            let mut rows = Vec::new();
            let mut seen = std::collections::HashSet::new();
            // The picker has an existing 1,024-row ceiling. Each server page is bounded and
            // authenticated by DirectoryClient; no unbounded enumeration is introduced.
            for _ in 0..MAXIMUM_DIRECTORY_ROWS / MAX_DIRECTORY_PAGE_ENTRIES {
                let page = client
                    .browse(directory.clone(), cursor)
                    .await
                    .map_err(source_error)?;
                for entry in &page.parts().entries {
                    if !entry.selectable {
                        continue;
                    }
                    let Ok(name) = std::str::from_utf8(entry.name.bytes()) else {
                        continue;
                    };
                    if seen.insert(name.to_owned())
                        && let Ok(row) = DirectoryRow::new(name.to_owned())
                    {
                        rows.push(row);
                    }
                }
                cursor = page.parts().next.clone();
                if cursor.is_none() {
                    return Ok(DirectoryListing::bounded(rows, false));
                }
            }
            Ok(DirectoryListing::bounded(rows, true))
        })
    }

    fn probe_exact_path(&self, directory: PickerPath) -> SourceFuture<ExactPathState> {
        self.run(|client| async move {
            let directory = resolve(&client, directory).await?;
            let validation = client
                .validate(directory, DirectoryPurpose::PaneRepair)
                .await
                .map_err(source_error)?;
            match validation.class {
                DirectoryValidationClass::AvailableDirectory => {
                    Ok(ExactPathState::ReadableDirectory)
                }
                DirectoryValidationClass::Missing
                | DirectoryValidationClass::AbsentCreateTarget => Ok(ExactPathState::Missing),
                DirectoryValidationClass::NotDirectory => Err(DirectorySourceError::NotDirectory),
                _ => Err(DirectorySourceError::Other),
            }
        })
    }

    fn create_directory_recursively(&self, _: PickerPath) -> SourceFuture<()> {
        Box::pin(async { Err(DirectorySourceError::Other) })
    }

    fn pin(&self, _: PickerPath) -> SourceFuture<PinnedDirectory> {
        Box::pin(async { Err(DirectorySourceError::Other) })
    }

    fn select(&self, directory: PickerPath) -> SourceFuture<DirectorySelection> {
        self.run(|client| async move {
            let directory = resolve(&client, directory).await?;
            let validated = client
                .validate(directory, DirectoryPurpose::PaneRepair)
                .await
                .map_err(source_error)?;
            match validated.class {
                DirectoryValidationClass::AvailableDirectory => {
                    Ok(DirectorySelection::Server(validated.directory))
                }
                DirectoryValidationClass::Missing => Err(DirectorySourceError::Missing),
                DirectoryValidationClass::NotDirectory => Err(DirectorySourceError::NotDirectory),
                _ => Err(DirectorySourceError::Other),
            }
        })
    }
}

async fn resolve(
    client: &DirectoryClient,
    directory: PickerPath,
) -> Result<ServerDirectory, DirectorySourceError> {
    let spelling = directory.as_str().trim_end_matches('/');
    let bytes = if let Some(relative) = spelling.strip_prefix('~') {
        let home = client.home().await.map_err(source_error)?;
        let mut bytes = home.bytes().to_vec();
        if !relative.is_empty() {
            if bytes.last() != Some(&b'/') {
                bytes.push(b'/');
            }
            bytes.extend_from_slice(relative.trim_start_matches('/').as_bytes());
        }
        bytes
    } else if spelling.is_empty() {
        b"/".to_vec()
    } else {
        spelling.as_bytes().to_vec()
    };
    ServerDirectory::new(client.server(), bytes).map_err(|_| DirectorySourceError::Other)
}

fn source_error(failure: ClientDirectoryFailure) -> DirectorySourceError {
    match failure {
        ClientDirectoryFailure::Offline
        | ClientDirectoryFailure::Reconciling
        | ClientDirectoryFailure::Unavailable => DirectorySourceError::ConnectionLost,
        ClientDirectoryFailure::Remote(DirectoryFailure::Missing) => DirectorySourceError::Missing,
        ClientDirectoryFailure::Remote(DirectoryFailure::NotDirectory) => {
            DirectorySourceError::NotDirectory
        }
        ClientDirectoryFailure::Remote(DirectoryFailure::Unauthorized) => {
            DirectorySourceError::PermissionDenied
        }
        _ => DirectorySourceError::Other,
    }
}

#[cfg(test)]
mod tests;
