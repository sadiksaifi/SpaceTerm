//! Owning-server reads through the application's authenticated directory port.
//! No chooser result, including one over private IPC, grants local path authority.

use std::{cell::RefCell, rc::Rc, sync::Arc};

use spaceterm_client::{
    directories::{ClientDirectoryFailure, DirectoryClient, DirectoryConnectionPort},
    git::rpc::{GitConnectionState, GitReadiness},
};
use spaceterm_core::{
    identity::ServerId,
    organization::ServerDirectory,
    server_directories::{
        DirectoryFailure, DirectoryPurpose, DirectoryValidationClass, MAX_DIRECTORY_PAGE_ENTRIES,
    },
};

use super::{
    DirectoryListing, DirectoryRow, DirectorySelection, DirectorySource, DirectorySourceError,
    ExactPathState, MAXIMUM_DIRECTORY_ROWS, PickerPath, SourceFuture,
};
use crate::domain::PinnedDirectory;

pub(crate) struct ServerDirectorySource {
    client: DirectoryClient,
    connection: Arc<dyn DirectoryConnectionPort>,
    name: String,
    purpose: DirectoryPurpose,
    home: Rc<RefCell<Option<ServerDirectory>>>,
    context: GitConnectionState,
}

impl ServerDirectorySource {
    pub(crate) fn new(
        server: ServerId,
        name: &str,
        connection: Arc<dyn DirectoryConnectionPort>,
        purpose: DirectoryPurpose,
    ) -> Result<Self, DirectorySourceError> {
        let client = DirectoryClient::new(server, connection.clone()).map_err(classify)?;
        let context = connection.state().map_err(classify)?;
        if context.readiness != GitReadiness::Ready {
            return Err(DirectorySourceError::ConnectionLost);
        }
        // This is a display label, never a route, path, or authorization input.
        let name = name.chars().filter(|c| !c.is_control()).take(128).collect();
        Ok(Self {
            client,
            connection,
            name,
            purpose,
            home: Rc::default(),
            context,
        })
    }

    fn ready(&self) -> Result<(), DirectorySourceError> {
        check_context(self.connection.as_ref(), self.context)
    }

    fn resolve(&self, path: PickerPath) -> Result<ServerDirectory, DirectorySourceError> {
        self.ready()?;
        let spelling = path.as_str();
        let value = if spelling == "~" || spelling.starts_with("~/") {
            let home = self.home.borrow();
            let home = home.as_ref().ok_or(DirectorySourceError::Other)?;
            let mut bytes = home.bytes().to_vec();
            while bytes.len() > 1 && bytes.last() == Some(&b'/') {
                bytes.pop();
            }
            let suffix = spelling.strip_prefix("~/").unwrap_or_default();
            if !suffix.is_empty() {
                if bytes.last() != Some(&b'/') {
                    bytes.push(b'/');
                }
                bytes.extend_from_slice(suffix.as_bytes());
            }
            bytes
        } else {
            spelling.as_bytes().to_vec()
        };
        ServerDirectory::new(self.client.server(), value).map_err(|_| DirectorySourceError::Other)
    }

    fn validation(&self, directory: PickerPath) -> SourceFuture<ServerDirectory> {
        let directory = self.resolve(directory);
        let client = self.client.clone();
        let connection = self.connection.clone();
        let context = self.context;
        let purpose = self.purpose;
        Box::pin(async move {
            let directory = directory?;
            let result = client
                .validate(directory.clone(), purpose)
                .await
                .map_err(classify)?;
            check_context(connection.as_ref(), context)?;
            match result.class {
                DirectoryValidationClass::AvailableDirectory => Ok(directory),
                DirectoryValidationClass::Missing
                | DirectoryValidationClass::AbsentCreateTarget => {
                    Err(DirectorySourceError::Missing)
                }
                DirectoryValidationClass::NotDirectory => Err(DirectorySourceError::NotDirectory),
                _ => Err(DirectorySourceError::Other),
            }
        })
    }
}

impl DirectorySource for ServerDirectorySource {
    fn machine_name(&self) -> Option<&str> {
        Some(&self.name)
    }
    fn is_server_backed(&self) -> bool {
        true
    }
    fn can_create(&self) -> bool {
        false
    }

    fn discover_home(&self) -> SourceFuture<PickerPath> {
        let ready = self.ready();
        let client = self.client.clone();
        let connection = self.connection.clone();
        let context = self.context;
        let home = self.home.clone();
        Box::pin(async move {
            ready?;
            let directory = client.home().await.map_err(classify)?;
            check_context(connection.as_ref(), context)?;
            let text =
                std::str::from_utf8(directory.bytes()).map_err(|_| DirectorySourceError::Other)?;
            let path = PickerPath::new(text.to_owned()).map_err(|_| DirectorySourceError::Other)?;
            if !path.as_str().starts_with('/') {
                return Err(DirectorySourceError::Other);
            }
            *home.borrow_mut() = Some(directory);
            Ok(path)
        })
    }

    fn list_directories(&self, directory: PickerPath) -> SourceFuture<DirectoryListing> {
        let directory = self.resolve(directory);
        let client = self.client.clone();
        let connection = self.connection.clone();
        let context = self.context;
        Box::pin(async move {
            let directory = directory?;
            let mut rows = Vec::new();
            let mut cursor = None;
            let mut observation = None;
            // Bound calls even if a peer returns empty pages with new cursors.
            for _ in 0..(MAXIMUM_DIRECTORY_ROWS / MAX_DIRECTORY_PAGE_ENTRIES) {
                let page = client
                    .browse(directory.clone(), cursor)
                    .await
                    .map_err(classify)?;
                check_context(connection.as_ref(), context)?;
                let page = page.parts();
                if observation.is_some_and(|previous| previous != page.observation) {
                    return Err(DirectorySourceError::Other);
                }
                observation = Some(page.observation);
                for entry in &page.entries {
                    if !entry.selectable {
                        continue;
                    }
                    let Ok(name) = std::str::from_utf8(entry.name.bytes()) else {
                        continue;
                    };
                    let Ok(row) = DirectoryRow::new(name.to_owned()) else {
                        continue;
                    };
                    let mut child = directory.bytes().to_vec();
                    while child.len() > 1 && child.last() == Some(&b'/') {
                        child.pop();
                    }
                    if child.last() != Some(&b'/') {
                        child.push(b'/');
                    }
                    child.extend_from_slice(entry.name.bytes());
                    if entry.directory.bytes() != child
                        || rows.iter().any(|r: &DirectoryRow| r.name() == name)
                    {
                        return Err(DirectorySourceError::Other);
                    }
                    rows.push(row);
                }
                cursor = page.next.clone();
                if cursor.is_none() {
                    return Ok(DirectoryListing::bounded(rows, false));
                }
            }
            Ok(DirectoryListing::bounded(rows, true))
        })
    }

    fn probe_exact_path(&self, directory: PickerPath) -> SourceFuture<ExactPathState> {
        let validation = self.validation(directory);
        Box::pin(async move {
            match validation.await {
                Ok(_) => Ok(ExactPathState::ReadableDirectory),
                Err(DirectorySourceError::Missing) => Ok(ExactPathState::Missing),
                Err(error) => Err(error),
            }
        })
    }

    fn select(&self, directory: PickerPath) -> SourceFuture<DirectorySelection> {
        let validation = self.validation(directory);
        Box::pin(async move { validation.await.map(DirectorySelection::Server) })
    }

    fn create_directory_recursively(&self, _: PickerPath) -> SourceFuture<()> {
        Box::pin(async { Err(DirectorySourceError::Other) })
    }

    fn pin(&self, _: PickerPath) -> SourceFuture<PinnedDirectory> {
        Box::pin(async { Err(DirectorySourceError::Other) })
    }
}

fn check_context(
    connection: &dyn DirectoryConnectionPort,
    expected: GitConnectionState,
) -> Result<(), DirectorySourceError> {
    if connection.state().map_err(classify)? != expected {
        return Err(DirectorySourceError::ConnectionLost);
    }
    Ok(())
}

fn classify(error: ClientDirectoryFailure) -> DirectorySourceError {
    match error {
        ClientDirectoryFailure::Offline
        | ClientDirectoryFailure::Reconciling
        | ClientDirectoryFailure::StaleResponse => DirectorySourceError::ConnectionLost,
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

/// The GPUI chooser retains local presentation state. Its authenticated query worker
/// runs on the native executor, where the connection's bounded read deadline lives.
pub(crate) fn on_executor(
    connection: Arc<dyn DirectoryConnectionPort>,
    executor: tokio::runtime::Handle,
) -> Arc<dyn DirectoryConnectionPort> {
    Arc::new(ExecutorConnection {
        connection,
        executor,
    })
}
struct ExecutorConnection {
    connection: Arc<dyn DirectoryConnectionPort>,
    executor: tokio::runtime::Handle,
}
struct CancelQuery(tokio::task::AbortHandle);
impl Drop for CancelQuery {
    fn drop(&mut self) {
        self.0.abort();
    }
}
impl DirectoryConnectionPort for ExecutorConnection {
    fn state(&self) -> Result<GitConnectionState, ClientDirectoryFailure> {
        self.connection.state()
    }
    fn query(
        &self,
        query: spaceterm_core::server_directories::DirectoryQuery,
    ) -> spaceterm_client::git::rpc::GitClientFuture<
        '_,
        Result<spaceterm_core::server_directories::DirectoryQueryResult, ClientDirectoryFailure>,
    > {
        let connection = self.connection.clone();
        let task = self
            .executor
            .spawn(async move { connection.query(query).await });
        let cancellation = CancelQuery(task.abort_handle());
        Box::pin(async move {
            let _cancellation = cancellation;
            task.await
                .map_err(|_| ClientDirectoryFailure::Unavailable)?
        })
    }
}
