use std::ffi::{OsStr, OsString};
use std::num::NonZeroUsize;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use thiserror::Error;

#[cfg(test)]
use super::app_directories::{APP_DIR_NAME, AppDirectoryEnvironment};
use super::app_directories::{AppDirectories, AppDirectoryFile, AppDirectoryRoot, DirectoryError};
use super::secure_filesystem::{
    SecureDirectory, SecureEntryIdentity, SecureFilesystem, SecureFilesystemError,
};

const RUNTIME_OWNER_CREATION_ATTEMPTS: usize = 128;
pub(crate) const ASKPASS_RUNTIME_OWNER_KIND: &str = "a";
pub(crate) const ASKPASS_RUNTIME_SOCKET_NAME: &str = "a";
pub(crate) const CONTROL_RUNTIME_OWNER_KIND: &str = "c";
pub(crate) const CONTROL_RUNTIME_SOCKET_NAME: &str = "c";
static NEXT_RUNTIME_OWNER: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
#[derive(Clone, Eq, PartialEq)]
pub(crate) struct AppPathHostFacts {
    runtime_fallback_root: Option<PathBuf>,
    local_ipc_path_maximum: NonZeroUsize,
}

#[cfg(test)]
impl std::fmt::Debug for AppPathHostFacts {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("AppPathHostFacts(..)")
    }
}

#[cfg(test)]
impl AppPathHostFacts {
    pub(crate) fn new(
        runtime_fallback_root: PathBuf,
        local_ipc_path_maximum: usize,
    ) -> Result<Self, AppPathHostFactsError> {
        if !is_absolute_normal_path(&runtime_fallback_root) {
            return Err(AppPathHostFactsError::InvalidRuntimeFallbackRoot);
        }
        let local_ipc_path_maximum = NonZeroUsize::new(local_ipc_path_maximum)
            .ok_or(AppPathHostFactsError::InvalidLocalIpcPathMaximum)?;
        Ok(Self {
            runtime_fallback_root: Some(runtime_fallback_root),
            local_ipc_path_maximum,
        })
    }

    /// Omits fallback capture when the startup environment already selects an absolute root.
    pub(crate) fn without_runtime_fallback(
        local_ipc_path_maximum: usize,
    ) -> Result<Self, AppPathHostFactsError> {
        let local_ipc_path_maximum = NonZeroUsize::new(local_ipc_path_maximum)
            .ok_or(AppPathHostFactsError::InvalidLocalIpcPathMaximum)?;
        Ok(Self {
            runtime_fallback_root: None,
            local_ipc_path_maximum,
        })
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(crate) enum AppPathHostFactsError {
    #[error("the runtime fallback root is invalid")]
    InvalidRuntimeFallbackRoot,
    #[error("the local IPC path maximum is invalid")]
    InvalidLocalIpcPathMaximum,
}

pub(crate) struct AppPaths {
    directories: AppDirectories,
    local_ipc_path_maximum: NonZeroUsize,
    filesystem: Arc<dyn SecureFilesystem>,
}

impl AppPaths {
    #[cfg(test)]
    pub(crate) fn resolve(
        environment: &AppDirectoryEnvironment,
        host: &AppPathHostFacts,
        filesystem: Arc<dyn SecureFilesystem>,
    ) -> Result<Self, AppPathsError> {
        let directories = AppDirectories::resolve_xdg(
            APP_DIR_NAME,
            environment,
            host.runtime_fallback_root.clone(),
        )?;
        Ok(Self {
            directories,
            local_ipc_path_maximum: host.local_ipc_path_maximum,
            filesystem,
        })
    }

    pub(crate) fn from_directories(
        directories: AppDirectories,
        local_ipc_path_maximum: usize,
        filesystem: Arc<dyn SecureFilesystem>,
    ) -> Result<Self, AppPathsError> {
        let local_ipc_path_maximum = NonZeroUsize::new(local_ipc_path_maximum)
            .ok_or(AppPathsError::InvalidLocalIpcPathMaximum)?;
        Ok(Self {
            directories,
            local_ipc_path_maximum,
            filesystem,
        })
    }

    pub(crate) fn directories(&self) -> &AppDirectories {
        &self.directories
    }

    #[cfg(test)]
    pub(crate) fn config(&self) -> &Path {
        &self.directories.config
    }
    #[cfg(test)]
    pub(crate) fn runtime(&self) -> &Path {
        self.directories.runtime.as_deref().unwrap()
    }

    pub(crate) fn managed_ssh_config(&self) -> PathBuf {
        self.directories.managed_ssh_config().into_path()
    }

    pub(crate) fn managed_ssh_config_file(&self) -> AppDirectoryFile {
        self.directories.managed_ssh_config()
    }

    pub(crate) fn open_secure_root(
        &self,
        root: AppDirectoryRoot,
    ) -> Result<Option<SecureDirectory>, AppPathsError> {
        self.filesystem
            .open_private_directory(self.directories.root(root))
            .map_err(Into::into)
    }

    pub(crate) fn ensure_secure_root(
        &self,
        root: AppDirectoryRoot,
    ) -> Result<SecureDirectory, AppPathsError> {
        self.filesystem
            .ensure_private_directory(self.directories.root(root))
            .map_err(Into::into)
    }

    pub(crate) fn filesystem(&self) -> &Arc<dyn SecureFilesystem> {
        &self.filesystem
    }

    pub(crate) fn create_runtime_owner(&self, kind: &str) -> Result<RuntimeOwner, AppPathsError> {
        validate_child_name(kind)?;
        let runtime_path = self
            .directories
            .runtime
            .as_ref()
            .ok_or(AppPathsError::RuntimeRootUnavailable)?
            .clone();
        let runtime = self.filesystem.ensure_private_directory(&runtime_path)?;
        for _ in 0..RUNTIME_OWNER_CREATION_ATTEMPTS {
            let sequence = NEXT_RUNTIME_OWNER.fetch_add(1, Ordering::Relaxed);
            let name = runtime_owner_name(kind, std::process::id(), sequence);
            match self
                .filesystem
                .create_private_child(&runtime, OsStr::new(&name))
            {
                Ok(directory) => {
                    return Ok(self.runtime_owner(runtime_path, runtime, name, directory));
                }
                Err(SecureFilesystemError::AlreadyExists) => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Err(AppPathsError::RuntimeOwnerExhausted)
    }

    #[cfg(test)]
    fn create_runtime_owner_with_identity(
        &self,
        kind: &str,
        process_id: u32,
        sequence: u64,
    ) -> Result<RuntimeOwner, AppPathsError> {
        validate_child_name(kind)?;
        let runtime_path = self
            .directories
            .runtime
            .as_ref()
            .ok_or(AppPathsError::RuntimeRootUnavailable)?;
        let runtime = self.filesystem.ensure_private_directory(runtime_path)?;
        let name = runtime_owner_name(kind, process_id, sequence);
        let directory = self
            .filesystem
            .create_private_child(&runtime, OsStr::new(&name))?;
        Ok(self.runtime_owner(runtime_path.clone(), runtime, name, directory))
    }

    fn runtime_owner(
        &self,
        runtime_path: PathBuf,
        runtime: SecureDirectory,
        name: String,
        directory: SecureDirectory,
    ) -> RuntimeOwner {
        RuntimeOwner {
            filesystem: Arc::clone(&self.filesystem),
            runtime,
            path: runtime_path.join(&name),
            name: OsString::from(name),
            directory,
            local_ipc_path_maximum: self.local_ipc_path_maximum,
            artifacts: Mutex::new(Vec::new()),
            closed: false,
        }
    }
}

impl std::fmt::Debug for AppPaths {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("AppPaths(..)")
    }
}

fn runtime_owner_name(kind: &str, process_id: u32, sequence: u64) -> String {
    format!(
        "{kind}-{}-{}",
        encode_base36(u64::from(process_id)),
        encode_base36(sequence)
    )
}

fn encode_base36(mut value: u64) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut reversed = [0_u8; 13];
    let mut length = 0;
    loop {
        reversed[length] = DIGITS[(value % 36) as usize];
        length += 1;
        value /= 36;
        if value == 0 {
            break;
        }
    }
    reversed[..length]
        .iter()
        .rev()
        .map(|byte| char::from(*byte))
        .collect()
}

pub(crate) struct RuntimeOwner {
    filesystem: Arc<dyn SecureFilesystem>,
    runtime: SecureDirectory,
    path: PathBuf,
    name: OsString,
    directory: SecureDirectory,
    local_ipc_path_maximum: NonZeroUsize,
    artifacts: Mutex<Vec<TrackedRuntimeArtifact>>,
    closed: bool,
}

impl RuntimeOwner {
    #[cfg(test)]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn socket_path(&self, name: &str) -> Result<PathBuf, AppPathsError> {
        validate_child_name(name)?;
        self.verify_identity()?;
        let path = self.path.join(name);
        let actual = path.as_os_str().as_encoded_bytes().len();
        let maximum = self.local_ipc_path_maximum.get();
        if actual > maximum {
            return Err(AppPathsError::LocalIpcAddressTooLong { actual, maximum });
        }
        Ok(path)
    }

    pub(crate) fn register_socket(
        &self,
        name: &str,
    ) -> Result<RegisteredRuntimeSocket, AppPathsError> {
        validate_child_name(name)?;
        self.verify_identity()?;
        let name = OsString::from(name);
        let identity = self.filesystem.register_socket(&self.directory, &name)?;
        if let Err(error) = self.verify_identity() {
            let _ = self
                .filesystem
                .remove_socket(&self.directory, &name, &identity);
            return Err(error);
        }
        self.artifacts
            .lock()
            .map_err(|_| AppPathsError::FilesystemUnavailable)?
            .push(TrackedRuntimeArtifact {
                name: name.clone(),
                socket_identity: Some(identity.clone()),
            });
        Ok(RegisteredRuntimeSocket {
            filesystem: Arc::clone(&self.filesystem),
            path: self.path.join(&name),
            name,
            socket_identity: identity,
            runtime: self.runtime.clone(),
            owner: self.directory.clone(),
        })
    }

    pub(crate) fn remove_registered_socket(
        &self,
        socket: RegisteredRuntimeSocket,
    ) -> Result<(), AppPathsError> {
        self.verify_identity()?;
        socket.verify()?;
        if socket.path.parent() != Some(self.path.as_path()) {
            return Err(AppPathsError::UnsafePath);
        }
        self.filesystem
            .remove_socket(&self.directory, &socket.name, &socket.socket_identity)?;
        self.artifacts
            .lock()
            .map_err(|_| AppPathsError::FilesystemUnavailable)?
            .retain(|artifact| artifact.name != socket.name);
        Ok(())
    }

    pub(crate) fn close(mut self) -> Result<(), AppPathsError> {
        let result = self.cleanup();
        if result.is_ok() {
            self.closed = true;
        }
        result
    }

    fn cleanup(&self) -> Result<(), AppPathsError> {
        self.verify_identity()?;
        let artifacts = self
            .artifacts
            .lock()
            .map_err(|_| AppPathsError::FilesystemUnavailable)?;
        for artifact in artifacts.iter() {
            let result = match &artifact.socket_identity {
                Some(identity) => {
                    self.filesystem
                        .remove_socket(&self.directory, &artifact.name, identity)
                }
                None => Err(SecureFilesystemError::Unsafe),
            };
            match result {
                Ok(()) | Err(SecureFilesystemError::Missing) => {}
                Err(error) => return Err(error.into()),
            }
        }
        drop(artifacts);
        self.filesystem
            .remove_private_child(&self.runtime, &self.name, &self.directory)?;
        Ok(())
    }

    fn verify_identity(&self) -> Result<(), AppPathsError> {
        self.filesystem.verify_directory(&self.runtime)?;
        self.filesystem.verify_directory(&self.directory)?;
        Ok(())
    }
}

impl Drop for RuntimeOwner {
    fn drop(&mut self) {
        if !self.closed {
            let _ = self.cleanup();
            self.closed = true;
        }
    }
}

#[derive(Clone)]
struct TrackedRuntimeArtifact {
    name: OsString,
    socket_identity: Option<SecureEntryIdentity>,
}

pub(crate) struct RegisteredRuntimeSocket {
    filesystem: Arc<dyn SecureFilesystem>,
    path: PathBuf,
    name: OsString,
    socket_identity: SecureEntryIdentity,
    runtime: SecureDirectory,
    owner: SecureDirectory,
}

impl RegisteredRuntimeSocket {
    pub(crate) fn verify(&self) -> Result<(), AppPathsError> {
        self.filesystem.verify_directory(&self.runtime)?;
        self.filesystem.verify_directory(&self.owner)?;
        self.filesystem
            .verify_socket(&self.owner, &self.name, &self.socket_identity)?;
        Ok(())
    }
}

impl std::fmt::Debug for RegisteredRuntimeSocket {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RegisteredRuntimeSocket(..)")
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(crate) enum AppPathsError {
    #[error(transparent)]
    Directories(#[from] DirectoryError),
    #[error("the local IPC path maximum is invalid")]
    InvalidLocalIpcPathMaximum,
    #[error("the application runtime root is unavailable")]
    RuntimeRootUnavailable,
    #[error("the application path is unsafe")]
    UnsafePath,
    #[error("the application filesystem is unavailable")]
    FilesystemUnavailable,
    #[error("failed to allocate a unique runtime owner directory")]
    RuntimeOwnerExhausted,
    #[error("invalid runtime artifact name")]
    InvalidArtifactName,
    #[error("the local IPC address uses {actual} bytes but permits at most {maximum}")]
    LocalIpcAddressTooLong { actual: usize, maximum: usize },
}

impl From<SecureFilesystemError> for AppPathsError {
    fn from(error: SecureFilesystemError) -> Self {
        match error {
            SecureFilesystemError::Unsafe => Self::UnsafePath,
            SecureFilesystemError::Missing
            | SecureFilesystemError::AlreadyExists
            | SecureFilesystemError::Unavailable => Self::FilesystemUnavailable,
        }
    }
}

#[cfg(test)]
fn is_absolute_normal_path(path: &Path) -> bool {
    path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
}

fn validate_child_name(name: &str) -> Result<(), AppPathsError> {
    let mut components = Path::new(name).components();
    let valid = !name.is_empty()
        && !name.as_bytes().contains(&0)
        && matches!(components.next(), Some(Component::Normal(_)))
        && components.next().is_none();
    if valid {
        Ok(())
    } else {
        Err(AppPathsError::InvalidArtifactName)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::app_directories::AppDirectoryEnvironment;

    use crate::platform::testing::RecordingFilesystem;
    fn environment() -> AppDirectoryEnvironment {
        AppDirectoryEnvironment {
            home: Some("/home/test".into()),
            ..Default::default()
        }
    }
    fn host(maximum: usize) -> AppPathHostFacts {
        AppPathHostFacts::new("/runtime".into(), maximum).unwrap()
    }

    #[test]
    fn resolve_should_apply_xdg_precedence_and_validate_spelling() {
        let environment = AppDirectoryEnvironment {
            xdg_config_home: Some("/explicit/../config".into()),
            xdg_runtime_dir: Some("relative".into()),
            ..environment()
        };
        let paths = AppPaths::resolve(
            &environment,
            &host(200),
            Arc::new(RecordingFilesystem::default()),
        )
        .unwrap();
        assert_eq!(paths.config(), Path::new("/explicit/../config/spaceterm"));
        assert_eq!(paths.runtime(), Path::new("/runtime/spaceterm"));
    }

    #[test]
    fn resolve_should_preserve_explicit_runtime_spelling_without_fallback_facts() {
        let environment = AppDirectoryEnvironment {
            xdg_runtime_dir: Some("/explicit/../runtime".into()),
            ..environment()
        };
        let host = AppPathHostFacts::without_runtime_fallback(200).unwrap();

        let paths = AppPaths::resolve(
            &environment,
            &host,
            Arc::new(RecordingFilesystem::default()),
        )
        .unwrap();

        assert_eq!(
            paths.runtime().as_os_str(),
            OsStr::new("/explicit/../runtime/spaceterm")
        );
    }

    #[test]
    fn runtime_owner_should_report_an_unavailable_runtime_directory() {
        let paths = AppPaths::resolve(
            &environment(),
            &AppPathHostFacts::without_runtime_fallback(200).unwrap(),
            Arc::new(RecordingFilesystem::default()),
        )
        .unwrap();

        assert!(matches!(
            paths.create_runtime_owner("a"),
            Err(AppPathsError::RuntimeRootUnavailable)
        ));
    }

    #[test]
    fn host_facts_should_reject_unusable_values() {
        assert_eq!(
            AppPathHostFacts::new("relative".into(), 1).unwrap_err(),
            AppPathHostFactsError::InvalidRuntimeFallbackRoot
        );
        assert_eq!(
            AppPathHostFacts::new("/runtime".into(), 0).unwrap_err(),
            AppPathHostFactsError::InvalidLocalIpcPathMaximum
        );
    }

    #[test]
    fn owner_policy_should_name_register_and_cleanup_in_order() {
        let filesystem = Arc::new(RecordingFilesystem::default());
        let paths = AppPaths::resolve(&environment(), &host(200), filesystem.clone()).unwrap();
        let owner = paths
            .create_runtime_owner_with_identity("a", 35, 36)
            .unwrap();
        assert!(owner.path().ends_with("a-z-10"));
        filesystem.create_socket(&owner.socket_path("a").unwrap());
        let socket = owner.register_socket("a").unwrap();
        owner.remove_registered_socket(socket).unwrap();
        owner.close().unwrap();
        let events = filesystem.events.lock().unwrap();
        assert!(
            events
                .iter()
                .position(|event| *event == "register")
                .unwrap()
                < events
                    .iter()
                    .position(|event| *event == "remove-socket")
                    .unwrap()
        );
        assert_eq!(events.last(), Some(&"remove-owner"));
    }

    #[test]
    fn socket_path_should_use_injected_constraint() {
        let filesystem = Arc::new(RecordingFilesystem::default());
        let paths = AppPaths::resolve(&environment(), &host(4), filesystem).unwrap();
        let owner = paths.create_runtime_owner_with_identity("a", 1, 1).unwrap();
        assert!(matches!(
            owner.socket_path("a"),
            Err(AppPathsError::LocalIpcAddressTooLong { .. })
        ));
    }

    #[test]
    fn debug_output_should_not_expose_paths_or_native_details() {
        let paths = AppPaths::resolve(
            &environment(),
            &host(200),
            Arc::new(RecordingFilesystem::default()),
        )
        .unwrap();
        assert_eq!(format!("{paths:?}"), "AppPaths(..)");
        assert_eq!(format!("{:?}", host(200)), "AppPathHostFacts(..)");
        assert_eq!(format!("{:?}", AppPathsError::UnsafePath), "UnsafePath");
    }
}
