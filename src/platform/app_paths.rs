use std::ffi::{OsStr, OsString};
use std::num::NonZeroUsize;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use thiserror::Error;

use super::secure_filesystem::{
    SecureDirectory, SecureEntryIdentity, SecureFilesystem, SecureFilesystemError,
};

const RUNTIME_OWNER_CREATION_ATTEMPTS: usize = 128;
pub(crate) const ASKPASS_RUNTIME_OWNER_KIND: &str = "a";
pub(crate) const ASKPASS_RUNTIME_SOCKET_NAME: &str = "a";
pub(crate) const CONTROL_RUNTIME_OWNER_KIND: &str = "c";
pub(crate) const CONTROL_RUNTIME_SOCKET_NAME: &str = "c";
const HOME_ENVIRONMENT_VARIABLE: &str = "HOME";
const XDG_CONFIG_HOME_ENVIRONMENT_VARIABLE: &str = "XDG_CONFIG_HOME";
const XDG_DATA_HOME_ENVIRONMENT_VARIABLE: &str = "XDG_DATA_HOME";
const XDG_STATE_HOME_ENVIRONMENT_VARIABLE: &str = "XDG_STATE_HOME";
const XDG_CACHE_HOME_ENVIRONMENT_VARIABLE: &str = "XDG_CACHE_HOME";
const XDG_RUNTIME_DIR_ENVIRONMENT_VARIABLE: &str = "XDG_RUNTIME_DIR";

static NEXT_RUNTIME_OWNER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Default, Eq, PartialEq)]
pub(crate) struct AppPathEnvironment {
    pub(crate) home: Option<OsString>,
    pub(crate) xdg_config_home: Option<OsString>,
    pub(crate) xdg_data_home: Option<OsString>,
    pub(crate) xdg_state_home: Option<OsString>,
    pub(crate) xdg_cache_home: Option<OsString>,
    pub(crate) xdg_runtime_dir: Option<OsString>,
}

impl std::fmt::Debug for AppPathEnvironment {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("AppPathEnvironment(<redacted>)")
    }
}

impl AppPathEnvironment {
    pub(crate) fn capture() -> Self {
        Self {
            home: std::env::var_os(HOME_ENVIRONMENT_VARIABLE),
            xdg_config_home: std::env::var_os(XDG_CONFIG_HOME_ENVIRONMENT_VARIABLE),
            xdg_data_home: std::env::var_os(XDG_DATA_HOME_ENVIRONMENT_VARIABLE),
            xdg_state_home: std::env::var_os(XDG_STATE_HOME_ENVIRONMENT_VARIABLE),
            xdg_cache_home: std::env::var_os(XDG_CACHE_HOME_ENVIRONMENT_VARIABLE),
            xdg_runtime_dir: std::env::var_os(XDG_RUNTIME_DIR_ENVIRONMENT_VARIABLE),
        }
    }

    pub(crate) fn configured_runtime_root(&self) -> Option<PathBuf> {
        absolute_environment_path(self.xdg_runtime_dir.as_deref())
    }
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct AppPathHostFacts {
    runtime_fallback_root: Option<PathBuf>,
    local_ipc_path_maximum: NonZeroUsize,
}

impl std::fmt::Debug for AppPathHostFacts {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("AppPathHostFacts(..)")
    }
}

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

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(crate) enum AppPathHostFactsError {
    #[error("the runtime fallback root is invalid")]
    InvalidRuntimeFallbackRoot,
    #[error("the local IPC path maximum is invalid")]
    InvalidLocalIpcPathMaximum,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AppPathRoot {
    Config,
    Data,
    State,
    Cache,
}

pub(crate) struct AppPaths {
    config: PathBuf,
    data: PathBuf,
    state: PathBuf,
    cache: PathBuf,
    runtime: PathBuf,
    local_ipc_path_maximum: NonZeroUsize,
    filesystem: Arc<dyn SecureFilesystem>,
}

impl AppPaths {
    pub(crate) fn resolve(
        environment: &AppPathEnvironment,
        host: &AppPathHostFacts,
        filesystem: Arc<dyn SecureFilesystem>,
    ) -> Result<Self, AppPathsError> {
        let home = absolute_environment_path(environment.home.as_deref());
        let config = resolve_root(
            environment.xdg_config_home.as_deref(),
            home.as_deref(),
            AppPathRoot::Config,
            &[".config"],
        )?;
        let data = resolve_root(
            environment.xdg_data_home.as_deref(),
            home.as_deref(),
            AppPathRoot::Data,
            &[".local", "share"],
        )?;
        let state = resolve_root(
            environment.xdg_state_home.as_deref(),
            home.as_deref(),
            AppPathRoot::State,
            &[".local", "state"],
        )?;
        let cache = resolve_root(
            environment.xdg_cache_home.as_deref(),
            home.as_deref(),
            AppPathRoot::Cache,
            &[".cache"],
        )?;
        let runtime_base = environment
            .configured_runtime_root()
            .or_else(|| host.runtime_fallback_root.clone())
            .ok_or(AppPathsError::RuntimeRootUnavailable)?;
        Ok(Self {
            config,
            data,
            state,
            cache,
            runtime: runtime_base.join("spaceterm"),
            local_ipc_path_maximum: host.local_ipc_path_maximum,
            filesystem,
        })
    }

    #[cfg(test)]
    pub(crate) fn config(&self) -> &Path {
        &self.config
    }
    #[cfg(test)]
    pub(crate) fn runtime(&self) -> &Path {
        &self.runtime
    }

    pub(crate) fn managed_ssh_config(&self) -> PathBuf {
        self.config.join("ssh_config")
    }

    pub(crate) fn open_secure_root(
        &self,
        root: AppPathRoot,
    ) -> Result<Option<SecureDirectory>, AppPathsError> {
        self.filesystem
            .open_private_directory(self.root(root))
            .map_err(Into::into)
    }

    pub(crate) fn ensure_secure_root(
        &self,
        root: AppPathRoot,
    ) -> Result<SecureDirectory, AppPathsError> {
        self.filesystem
            .ensure_private_directory(self.root(root))
            .map_err(Into::into)
    }

    pub(crate) fn filesystem(&self) -> &Arc<dyn SecureFilesystem> {
        &self.filesystem
    }

    pub(crate) fn create_runtime_owner(&self, kind: &str) -> Result<RuntimeOwner, AppPathsError> {
        validate_child_name(kind)?;
        let runtime = self.filesystem.ensure_private_directory(&self.runtime)?;
        for _ in 0..RUNTIME_OWNER_CREATION_ATTEMPTS {
            let sequence = NEXT_RUNTIME_OWNER.fetch_add(1, Ordering::Relaxed);
            let name = runtime_owner_name(kind, std::process::id(), sequence);
            match self
                .filesystem
                .create_private_child(&runtime, OsStr::new(&name))
            {
                Ok(directory) => return Ok(self.runtime_owner(runtime, name, directory)),
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
        let runtime = self.filesystem.ensure_private_directory(&self.runtime)?;
        let name = runtime_owner_name(kind, process_id, sequence);
        let directory = self
            .filesystem
            .create_private_child(&runtime, OsStr::new(&name))?;
        Ok(self.runtime_owner(runtime, name, directory))
    }

    fn runtime_owner(
        &self,
        runtime: SecureDirectory,
        name: String,
        directory: SecureDirectory,
    ) -> RuntimeOwner {
        RuntimeOwner {
            filesystem: Arc::clone(&self.filesystem),
            runtime,
            path: self.runtime.join(&name),
            name: OsString::from(name),
            directory,
            local_ipc_path_maximum: self.local_ipc_path_maximum,
            artifacts: Mutex::new(Vec::new()),
            closed: false,
        }
    }

    fn root(&self, root: AppPathRoot) -> &Path {
        match root {
            AppPathRoot::Config => &self.config,
            AppPathRoot::Data => &self.data,
            AppPathRoot::State => &self.state,
            AppPathRoot::Cache => &self.cache,
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
    #[error("HOME is required to resolve the {root:?} application root")]
    MissingHome { root: AppPathRoot },
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

fn absolute_environment_path(value: Option<&OsStr>) -> Option<PathBuf> {
    value
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

fn is_absolute_normal_path(path: &Path) -> bool {
    path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
}

fn resolve_root(
    configured: Option<&OsStr>,
    home: Option<&Path>,
    root: AppPathRoot,
    fallback_components: &[&str],
) -> Result<PathBuf, AppPathsError> {
    let base = match absolute_environment_path(configured) {
        Some(configured) => configured,
        None => {
            let home = home.ok_or(AppPathsError::MissingHome { root })?;
            fallback_components
                .iter()
                .fold(home.to_path_buf(), |path, component| path.join(component))
        }
    };
    Ok(base.join("spaceterm"))
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
    use std::collections::BTreeSet;

    use super::*;
    use crate::platform::secure_filesystem::{
        PreparedPrivateFile, PrivateFileSnapshot, SecureCommitOutcome,
    };

    #[test]
    fn captured_environment_debug_should_redact_paths() {
        let environment = AppPathEnvironment {
            home: Some("/sensitive/home".into()),
            xdg_runtime_dir: Some("/sensitive/runtime".into()),
            ..AppPathEnvironment::default()
        };

        let debug = format!("{environment:?}");
        assert_eq!(debug, "AppPathEnvironment(<redacted>)");
        assert!(!debug.contains("sensitive"));
    }

    #[derive(Default)]
    struct RecordingFilesystem {
        directories: Mutex<BTreeSet<PathBuf>>,
        events: Mutex<Vec<&'static str>>,
    }
    #[derive(Clone)]
    struct RecordingDirectory(PathBuf);
    #[derive(Clone)]
    struct RecordingIdentity;

    impl RecordingFilesystem {
        fn path(directory: &SecureDirectory) -> Result<&PathBuf, SecureFilesystemError> {
            directory
                .0
                .downcast_ref::<RecordingDirectory>()
                .map(|directory| &directory.0)
                .ok_or(SecureFilesystemError::Unsafe)
        }
        fn directory(path: PathBuf) -> SecureDirectory {
            SecureDirectory(Arc::new(RecordingDirectory(path)))
        }
    }

    impl SecureFilesystem for RecordingFilesystem {
        fn open_private_directory(
            &self,
            path: &Path,
        ) -> Result<Option<SecureDirectory>, SecureFilesystemError> {
            Ok(self
                .directories
                .lock()
                .unwrap()
                .contains(path)
                .then(|| Self::directory(path.to_path_buf())))
        }
        fn ensure_private_directory(
            &self,
            path: &Path,
        ) -> Result<SecureDirectory, SecureFilesystemError> {
            self.events.lock().unwrap().push("ensure");
            self.directories.lock().unwrap().insert(path.to_path_buf());
            Ok(Self::directory(path.to_path_buf()))
        }
        fn create_private_child(
            &self,
            parent: &SecureDirectory,
            name: &OsStr,
        ) -> Result<SecureDirectory, SecureFilesystemError> {
            self.events.lock().unwrap().push("create-owner");
            let path = Self::path(parent)?.join(name);
            if !self.directories.lock().unwrap().insert(path.clone()) {
                return Err(SecureFilesystemError::AlreadyExists);
            }
            Ok(Self::directory(path))
        }
        fn verify_directory(&self, _: &SecureDirectory) -> Result<(), SecureFilesystemError> {
            self.events.lock().unwrap().push("verify");
            Ok(())
        }
        fn remove_private_child(
            &self,
            _: &SecureDirectory,
            _: &OsStr,
            _: &SecureDirectory,
        ) -> Result<(), SecureFilesystemError> {
            self.events.lock().unwrap().push("remove-owner");
            Ok(())
        }
        fn read_private_file(
            &self,
            _: &SecureDirectory,
            _: &OsStr,
            _: usize,
        ) -> Result<Option<PrivateFileSnapshot>, SecureFilesystemError> {
            Ok(None)
        }
        fn prepare_private_file(
            &self,
            _: &SecureDirectory,
            _: &OsStr,
            _: &[u8],
            _: [u8; 16],
        ) -> Result<PreparedPrivateFile, SecureFilesystemError> {
            Ok(PreparedPrivateFile(Box::new(())))
        }
        fn commit_private_file(
            &self,
            _: PreparedPrivateFile,
            _: Option<&SecureEntryIdentity>,
        ) -> Result<SecureCommitOutcome, SecureFilesystemError> {
            Ok(SecureCommitOutcome::Committed)
        }
        fn register_socket(
            &self,
            _: &SecureDirectory,
            _: &OsStr,
        ) -> Result<SecureEntryIdentity, SecureFilesystemError> {
            self.events.lock().unwrap().push("register");
            Ok(SecureEntryIdentity(Arc::new(RecordingIdentity)))
        }
        fn verify_socket(
            &self,
            _: &SecureDirectory,
            _: &OsStr,
            _: &SecureEntryIdentity,
        ) -> Result<(), SecureFilesystemError> {
            self.events.lock().unwrap().push("verify-socket");
            Ok(())
        }
        fn remove_socket(
            &self,
            _: &SecureDirectory,
            _: &OsStr,
            _: &SecureEntryIdentity,
        ) -> Result<(), SecureFilesystemError> {
            self.events.lock().unwrap().push("remove-socket");
            Ok(())
        }
        fn create_private_artifact(
            &self,
            _: &SecureDirectory,
            _: &OsStr,
        ) -> Result<(), SecureFilesystemError> {
            Ok(())
        }
    }

    fn environment() -> AppPathEnvironment {
        AppPathEnvironment {
            home: Some("/home/test".into()),
            ..Default::default()
        }
    }
    fn host(maximum: usize) -> AppPathHostFacts {
        AppPathHostFacts::new("/runtime".into(), maximum).unwrap()
    }

    #[test]
    fn resolve_should_apply_xdg_precedence_and_validate_spelling() {
        let environment = AppPathEnvironment {
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
        let environment = AppPathEnvironment {
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
    fn resolve_should_reject_missing_runtime_root_and_fallback() {
        let result = AppPaths::resolve(
            &environment(),
            &AppPathHostFacts::without_runtime_fallback(200).unwrap(),
            Arc::new(RecordingFilesystem::default()),
        );

        assert!(matches!(result, Err(AppPathsError::RuntimeRootUnavailable)));
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
