use std::cell::RefCell;
use std::ffi::{CString, OsStr, OsString};
use std::fs::{self, File};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::io::{AsRawFd, FromRawFd};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use thiserror::Error;

const MACOS_UNIX_SOCKET_PATH_BYTES: usize = 103;
const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
const PRIVATE_ARTIFACT_MODE: u32 = 0o600;
const RUNTIME_OWNER_CREATION_ATTEMPTS: usize = 128;
const HOME_ENVIRONMENT_VARIABLE: &str = "HOME";
const XDG_CONFIG_HOME_ENVIRONMENT_VARIABLE: &str = "XDG_CONFIG_HOME";
const XDG_DATA_HOME_ENVIRONMENT_VARIABLE: &str = "XDG_DATA_HOME";
const XDG_STATE_HOME_ENVIRONMENT_VARIABLE: &str = "XDG_STATE_HOME";
const XDG_CACHE_HOME_ENVIRONMENT_VARIABLE: &str = "XDG_CACHE_HOME";
const XDG_RUNTIME_DIR_ENVIRONMENT_VARIABLE: &str = "XDG_RUNTIME_DIR";

static NEXT_RUNTIME_OWNER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct AppPathEnvironment {
    pub(crate) home: Option<OsString>,
    pub(crate) xdg_config_home: Option<OsString>,
    pub(crate) xdg_data_home: Option<OsString>,
    pub(crate) xdg_state_home: Option<OsString>,
    pub(crate) xdg_cache_home: Option<OsString>,
    pub(crate) xdg_runtime_dir: Option<OsString>,
    pub(crate) macos_temporary_directory: PathBuf,
}

trait AppPathEnvironmentReader {
    fn environment_variable(&mut self, key: &OsStr) -> Option<OsString>;

    fn macos_temporary_directory(&mut self) -> PathBuf;
}

struct ProcessAppPathEnvironmentReader;

impl AppPathEnvironmentReader for ProcessAppPathEnvironmentReader {
    fn environment_variable(&mut self, key: &OsStr) -> Option<OsString> {
        std::env::var_os(key)
    }

    fn macos_temporary_directory(&mut self) -> PathBuf {
        canonicalize_macos_temporary_directory(std::env::temp_dir())
    }
}

/// Resolves macOS system aliases such as `/var` only for the trusted temporary-directory fallback.
/// Explicit XDG roots remain untouched and are still traversed without following symlinks.
fn canonicalize_macos_temporary_directory(path: PathBuf) -> PathBuf {
    fs::canonicalize(&path).unwrap_or(path)
}

impl AppPathEnvironment {
    pub(crate) fn capture() -> Self {
        Self::capture_with(&mut ProcessAppPathEnvironmentReader)
    }

    fn capture_with(reader: &mut impl AppPathEnvironmentReader) -> Self {
        Self {
            home: reader.environment_variable(OsStr::new(HOME_ENVIRONMENT_VARIABLE)),
            xdg_config_home: reader
                .environment_variable(OsStr::new(XDG_CONFIG_HOME_ENVIRONMENT_VARIABLE)),
            xdg_data_home: reader
                .environment_variable(OsStr::new(XDG_DATA_HOME_ENVIRONMENT_VARIABLE)),
            xdg_state_home: reader
                .environment_variable(OsStr::new(XDG_STATE_HOME_ENVIRONMENT_VARIABLE)),
            xdg_cache_home: reader
                .environment_variable(OsStr::new(XDG_CACHE_HOME_ENVIRONMENT_VARIABLE)),
            xdg_runtime_dir: reader
                .environment_variable(OsStr::new(XDG_RUNTIME_DIR_ENVIRONMENT_VARIABLE)),
            macos_temporary_directory: reader.macos_temporary_directory(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AppPathRoot {
    Config,
    Data,
    State,
    Cache,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AppPaths {
    config: PathBuf,
    data: PathBuf,
    state: PathBuf,
    cache: PathBuf,
    runtime: PathBuf,
}

impl AppPaths {
    pub(crate) fn resolve(environment: &AppPathEnvironment) -> Result<Self, AppPathsError> {
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
        let runtime_base = absolute_environment_path(environment.xdg_runtime_dir.as_deref())
            .unwrap_or_else(|| environment.macos_temporary_directory.clone());
        if !runtime_base.is_absolute() {
            return Err(AppPathsError::InvalidTemporaryDirectory);
        }

        Ok(Self {
            config,
            data,
            state,
            cache,
            runtime: runtime_base.join("spaceterm"),
        })
    }

    pub(crate) fn config(&self) -> &Path {
        &self.config
    }

    #[cfg(test)]
    pub(crate) fn data(&self) -> &Path {
        &self.data
    }

    #[cfg(test)]
    pub(crate) fn state(&self) -> &Path {
        &self.state
    }

    #[cfg(test)]
    pub(crate) fn cache(&self) -> &Path {
        &self.cache
    }

    #[cfg(test)]
    pub(crate) fn runtime(&self) -> &Path {
        &self.runtime
    }

    pub(crate) fn managed_ssh_config(&self) -> PathBuf {
        self.config.join("ssh_config")
    }

    pub(crate) fn ensure_root(&self, root: AppPathRoot) -> Result<&Path, AppPathsError> {
        let path = self.root(root);
        ensure_private_directory(path)?;
        Ok(path)
    }

    pub(crate) fn create_runtime_owner(&self, kind: &str) -> Result<RuntimeOwner, AppPathsError> {
        validate_child_name(kind)?;
        let runtime = ensure_private_directory(&self.runtime)?;
        for _ in 0..RUNTIME_OWNER_CREATION_ATTEMPTS {
            let sequence = NEXT_RUNTIME_OWNER.fetch_add(1, Ordering::Relaxed);
            let name = format!("{kind}-{}-{sequence:016x}", std::process::id());
            let path = self.runtime.join(&name);
            match create_directory_at(&runtime.file, OsStr::new(&name)) {
                Ok(()) => {
                    let directory =
                        match open_private_child_directory(&runtime.file, OsStr::new(&name), &path)
                        {
                            Ok(directory) => directory,
                            Err(error) => {
                                let _ = remove_directory_at(&runtime.file, OsStr::new(&name));
                                return Err(error);
                            }
                        };
                    return Ok(RuntimeOwner {
                        runtime,
                        path,
                        name: OsString::from(name),
                        directory,
                        artifacts: RefCell::new(Vec::new()),
                        closed: false,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(source) => {
                    return Err(AppPathsError::CreateDirectory { path, source });
                }
            }
        }
        Err(AppPathsError::RuntimeOwnerExhausted)
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

pub(crate) struct RuntimeOwner {
    runtime: PrivateDirectory,
    path: PathBuf,
    name: OsString,
    directory: PrivateDirectory,
    artifacts: RefCell<Vec<TrackedRuntimeArtifact>>,
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
        let actual = path.as_os_str().as_bytes().len();
        if actual > MACOS_UNIX_SOCKET_PATH_BYTES {
            return Err(AppPathsError::SocketPathTooLong {
                actual,
                maximum: MACOS_UNIX_SOCKET_PATH_BYTES,
            });
        }
        Ok(path)
    }

    #[cfg(test)]
    pub(crate) fn create_artifact(&self, name: &str) -> Result<RuntimeArtifact, AppPathsError> {
        validate_child_name(name)?;
        self.verify_identity()?;
        let path = self.path.join(name);
        let artifact_name = OsStr::new(name);
        let file = create_file_at(&self.directory.file, artifact_name, &path)?;
        let mut rollback = ArtifactRollback::new(&self.directory.file, artifact_name);
        set_file_mode(&file, PRIVATE_ARTIFACT_MODE).map_err(|source| {
            AppPathsError::SetPermissions {
                path: path.clone(),
                source,
            }
        })?;
        let metadata = file
            .metadata()
            .map_err(|source| AppPathsError::InspectPath {
                path: path.clone(),
                source,
            })?;
        if !metadata.is_file()
            || metadata.uid() != effective_user_id()
            || metadata.mode() & 0o777 != PRIVATE_ARTIFACT_MODE
        {
            return Err(AppPathsError::UnsafePath { path });
        }
        self.verify_identity()?;
        self.artifacts.borrow_mut().push(TrackedRuntimeArtifact {
            name: artifact_name.to_os_string(),
            socket_identity: None,
        });
        rollback.disarm();
        Ok(RuntimeArtifact { path, file })
    }

    pub(crate) fn register_socket(
        &self,
        name: &str,
    ) -> Result<RegisteredRuntimeSocket, AppPathsError> {
        validate_child_name(name)?;
        self.verify_identity()?;
        let path = self.path.join(name);
        let socket_name = OsStr::new(name);
        let before = inspect_socket_entry_at(&self.directory.file, socket_name, &path, None)?;
        set_entry_mode_at(&self.directory.file, socket_name, PRIVATE_ARTIFACT_MODE).map_err(
            |source| AppPathsError::SetPermissions {
                path: path.clone(),
                source,
            },
        )?;
        let after = inspect_socket_entry_at(
            &self.directory.file,
            socket_name,
            &path,
            Some(PRIVATE_ARTIFACT_MODE),
        )?;
        if before != after {
            return Err(AppPathsError::UnsafePath { path });
        }
        if let Err(error) = self.verify_identity() {
            let _ = remove_file_at(&self.directory.file, socket_name);
            return Err(error);
        }
        let registration = RegisteredRuntimeSocket::new(
            path.clone(),
            socket_name.to_os_string(),
            after,
            &self.runtime,
            &self.directory,
        )?;
        self.artifacts.borrow_mut().push(TrackedRuntimeArtifact {
            name: socket_name.to_os_string(),
            socket_identity: Some(after),
        });
        Ok(registration)
    }

    pub(crate) fn remove_registered_socket(
        &self,
        socket: RegisteredRuntimeSocket,
    ) -> Result<(), AppPathsError> {
        self.verify_identity()?;
        socket.verify()?;
        if socket.path.parent() != Some(self.path.as_path()) {
            return Err(AppPathsError::UnsafePath {
                path: socket.path.clone(),
            });
        }
        remove_file_at(&self.directory.file, &socket.name).map_err(|source| {
            AppPathsError::Cleanup {
                path: socket.path.clone(),
                source,
            }
        })?;
        self.artifacts.borrow_mut().retain(|artifact| {
            artifact.name != socket.name || artifact.socket_identity != Some(socket.socket_identity)
        });
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
        for artifact in self.artifacts.borrow().iter() {
            if let Some(expected_identity) = artifact.socket_identity {
                let path = self.path.join(&artifact.name);
                match inspect_socket_entry_at(
                    &self.directory.file,
                    &artifact.name,
                    &path,
                    Some(PRIVATE_ARTIFACT_MODE),
                ) {
                    Ok(identity) if identity == expected_identity => {}
                    Ok(_) => return Err(AppPathsError::UnsafePath { path }),
                    Err(AppPathsError::InspectPath { source, .. })
                        if source.kind() == io::ErrorKind::NotFound =>
                    {
                        continue;
                    }
                    Err(error) => return Err(error),
                }
            }
            match remove_file_at(&self.directory.file, &artifact.name) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(AppPathsError::Cleanup {
                        path: self.path.join(&artifact.name),
                        source,
                    });
                }
            }
        }
        self.verify_identity()?;
        remove_directory_at(&self.runtime.file, &self.name).map_err(|source| {
            AppPathsError::Cleanup {
                path: self.path.clone(),
                source,
            }
        })
    }

    fn verify_identity(&self) -> Result<(), AppPathsError> {
        if self.path.parent() != Some(self.runtime.path.as_path()) {
            return Err(AppPathsError::UnsafePath {
                path: self.path.clone(),
            });
        }
        self.runtime.verify_entry()?;
        self.directory.verify_entry()
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

#[cfg(test)]
pub(crate) struct RuntimeArtifact {
    path: PathBuf,
    file: File,
}

pub(crate) struct RegisteredRuntimeSocket {
    path: PathBuf,
    name: OsString,
    socket_identity: DirectoryIdentity,
    runtime: RegisteredDirectory,
    owner: RegisteredDirectory,
}

impl RegisteredRuntimeSocket {
    fn new(
        path: PathBuf,
        name: OsString,
        socket_identity: DirectoryIdentity,
        runtime: &PrivateDirectory,
        owner: &PrivateDirectory,
    ) -> Result<Self, AppPathsError> {
        Ok(Self {
            path,
            name,
            socket_identity,
            runtime: RegisteredDirectory::new(runtime)?,
            owner: RegisteredDirectory::new(owner)?,
        })
    }

    #[cfg(test)]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn verify(&self) -> Result<(), AppPathsError> {
        self.runtime.verify()?;
        self.owner.verify()?;
        if self.path.parent() != Some(self.owner.path.as_path()) {
            return Err(AppPathsError::UnsafePath {
                path: self.path.clone(),
            });
        }
        let identity = inspect_socket_entry_at(
            &self.owner.file,
            &self.name,
            &self.path,
            Some(PRIVATE_ARTIFACT_MODE),
        )?;
        if identity == self.socket_identity {
            Ok(())
        } else {
            Err(AppPathsError::UnsafePath {
                path: self.path.clone(),
            })
        }
    }
}

impl std::fmt::Debug for RegisteredRuntimeSocket {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RegisteredRuntimeSocket")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

struct RegisteredDirectory {
    parent: File,
    file: File,
    name: OsString,
    path: PathBuf,
    identity: DirectoryIdentity,
}

impl RegisteredDirectory {
    fn new(directory: &PrivateDirectory) -> Result<Self, AppPathsError> {
        Ok(Self {
            parent: directory
                .parent
                .try_clone()
                .map_err(|source| AppPathsError::InspectPath {
                    path: directory.path.clone(),
                    source,
                })?,
            file: directory
                .file
                .try_clone()
                .map_err(|source| AppPathsError::InspectPath {
                    path: directory.path.clone(),
                    source,
                })?,
            name: directory.name.clone(),
            path: directory.path.clone(),
            identity: directory.identity,
        })
    }

    fn verify(&self) -> Result<(), AppPathsError> {
        let entry = open_directory_at(&self.parent, &self.name).map_err(|source| {
            if matches!(
                source.raw_os_error(),
                Some(code) if code == libc::ELOOP || code == libc::ENOTDIR || code == libc::ENOENT
            ) {
                AppPathsError::UnsafePath {
                    path: self.path.clone(),
                }
            } else {
                AppPathsError::InspectPath {
                    path: self.path.clone(),
                    source,
                }
            }
        })?;
        let entry_identity = validate_private_open_directory(&entry, &self.path)?;
        let open_identity = validate_private_open_directory(&self.file, &self.path)?;
        if entry_identity == self.identity && open_identity == self.identity {
            Ok(())
        } else {
            Err(AppPathsError::UnsafePath {
                path: self.path.clone(),
            })
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TrackedRuntimeArtifact {
    name: OsString,
    socket_identity: Option<DirectoryIdentity>,
}

#[cfg(test)]
impl RuntimeArtifact {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn file(&self) -> &File {
        &self.file
    }
}

#[derive(Debug, Error)]
pub(crate) enum AppPathsError {
    #[error("HOME is required to resolve the {root:?} application root")]
    MissingHome { root: AppPathRoot },
    #[error("the macOS user temporary directory is not absolute")]
    InvalidTemporaryDirectory,
    #[error("the application path is unsafe: {}", path.display())]
    UnsafePath { path: PathBuf },
    #[error("failed to create application directory {}: {source}", path.display())]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to inspect application path {}: {source}", path.display())]
    InspectPath {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to restrict application path {}: {source}", path.display())]
    SetPermissions {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to allocate a unique runtime owner directory")]
    RuntimeOwnerExhausted,
    #[error("invalid runtime artifact name")]
    InvalidArtifactName,
    #[error("the Unix socket path uses {actual} bytes but macOS permits at most {maximum}")]
    SocketPathTooLong { actual: usize, maximum: usize },
    #[cfg(test)]
    #[error("failed to create runtime artifact {}: {source}", path.display())]
    CreateArtifact {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to clean runtime owner {}: {source}", path.display())]
    Cleanup {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DirectoryIdentity {
    device: u64,
    inode: u64,
}

struct PrivateDirectory {
    parent: File,
    file: File,
    name: OsString,
    path: PathBuf,
    identity: DirectoryIdentity,
}

impl PrivateDirectory {
    fn verify_entry(&self) -> Result<(), AppPathsError> {
        let entry = open_directory_at(&self.parent, &self.name).map_err(|source| {
            if matches!(
                source.raw_os_error(),
                Some(code)
                    if code == libc::ELOOP
                        || code == libc::ENOTDIR
                        || code == libc::ENOENT
            ) {
                AppPathsError::UnsafePath {
                    path: self.path.clone(),
                }
            } else {
                AppPathsError::InspectPath {
                    path: self.path.clone(),
                    source,
                }
            }
        })?;
        let entry_identity = validate_private_open_directory(&entry, &self.path)?;
        let open_identity = validate_private_open_directory(&self.file, &self.path)?;
        if entry_identity == self.identity && open_identity == self.identity {
            Ok(())
        } else {
            Err(AppPathsError::UnsafePath {
                path: self.path.clone(),
            })
        }
    }
}

struct DirectoryCreationRollback {
    entries: Vec<(File, OsString)>,
    armed: bool,
}

impl DirectoryCreationRollback {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
            armed: true,
        }
    }

    fn record(&mut self, parent: &File, name: &OsStr) -> io::Result<()> {
        self.entries
            .push((parent.try_clone()?, name.to_os_string()));
        Ok(())
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for DirectoryCreationRollback {
    fn drop(&mut self) {
        if self.armed {
            for (parent, name) in self.entries.iter().rev() {
                let _ = remove_directory_at(parent, name);
            }
        }
    }
}

#[cfg(test)]
struct ArtifactRollback<'a> {
    parent: &'a File,
    name: OsString,
    armed: bool,
}

#[cfg(test)]
impl<'a> ArtifactRollback<'a> {
    fn new(parent: &'a File, name: &OsStr) -> Self {
        Self {
            parent,
            name: name.to_os_string(),
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

#[cfg(test)]
impl Drop for ArtifactRollback<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = remove_file_at(self.parent, &self.name);
        }
    }
}

impl DirectoryIdentity {
    fn from_metadata(metadata: &fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

fn absolute_environment_path(value: Option<&OsStr>) -> Option<PathBuf> {
    value
        .filter(|value| !value.as_bytes().is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
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

fn ensure_private_directory(path: &Path) -> Result<PrivateDirectory, AppPathsError> {
    if !path.is_absolute() {
        return Err(AppPathsError::UnsafePath {
            path: path.to_path_buf(),
        });
    }
    let mut parent = File::open("/").map_err(|source| AppPathsError::InspectPath {
        path: PathBuf::from("/"),
        source,
    })?;
    let mut traversed = PathBuf::from("/");
    let mut components = path.components().peekable();
    let mut rollback = DirectoryCreationRollback::new();
    while let Some(component) = components.next() {
        let Component::Normal(name) = component else {
            if matches!(component, Component::RootDir) {
                continue;
            }
            return Err(AppPathsError::UnsafePath {
                path: path.to_path_buf(),
            });
        };
        traversed.push(name);
        let is_final = components.peek().is_none();
        let (directory, created) = match open_directory_at(&parent, name) {
            Ok(directory) => (directory, false),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let created = match create_directory_at(&parent, name) {
                    Ok(()) => {
                        if let Err(source) = rollback.record(&parent, name) {
                            let _ = remove_directory_at(&parent, name);
                            return Err(AppPathsError::InspectPath {
                                path: traversed.clone(),
                                source,
                            });
                        }
                        true
                    }
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => false,
                    Err(source) => {
                        return Err(AppPathsError::CreateDirectory {
                            path: traversed.clone(),
                            source,
                        });
                    }
                };
                let directory = open_directory_at(&parent, name).map_err(|source| {
                    if matches!(source.raw_os_error(), Some(code) if code == libc::ELOOP || code == libc::ENOTDIR)
                    {
                        AppPathsError::UnsafePath {
                            path: traversed.clone(),
                        }
                    } else {
                        AppPathsError::InspectPath {
                            path: traversed.clone(),
                            source,
                        }
                    }
                })?;
                if created {
                    set_file_mode(&directory, PRIVATE_DIRECTORY_MODE).map_err(|source| {
                        AppPathsError::SetPermissions {
                            path: traversed.clone(),
                            source,
                        }
                    })?;
                }
                validate_private_open_directory(&directory, &traversed).map_err(|error| {
                    if created {
                        error
                    } else {
                        AppPathsError::UnsafePath {
                            path: traversed.clone(),
                        }
                    }
                })?;
                if !created && is_final {
                    let metadata =
                        directory
                            .metadata()
                            .map_err(|source| AppPathsError::InspectPath {
                                path: traversed.clone(),
                                source,
                            })?;
                    if metadata.uid() != effective_user_id() {
                        return Err(AppPathsError::UnsafePath {
                            path: traversed.clone(),
                        });
                    }
                }
                (directory, created)
            }
            Err(source) if matches!(source.raw_os_error(), Some(code) if code == libc::ELOOP || code == libc::ENOTDIR) =>
            {
                return Err(AppPathsError::UnsafePath {
                    path: traversed.clone(),
                });
            }
            Err(source) => {
                return Err(AppPathsError::InspectPath {
                    path: traversed.clone(),
                    source,
                });
            }
        };
        if is_final {
            if !created {
                let metadata =
                    directory
                        .metadata()
                        .map_err(|source| AppPathsError::InspectPath {
                            path: traversed.clone(),
                            source,
                        })?;
                if !metadata.is_dir() || metadata.uid() != effective_user_id() {
                    return Err(AppPathsError::UnsafePath {
                        path: traversed.clone(),
                    });
                }
                set_file_mode(&directory, PRIVATE_DIRECTORY_MODE).map_err(|source| {
                    AppPathsError::SetPermissions {
                        path: traversed.clone(),
                        source,
                    }
                })?;
            }
            let identity = validate_private_open_directory(&directory, &traversed)?;
            rollback.disarm();
            return Ok(PrivateDirectory {
                parent,
                file: directory,
                name: name.to_os_string(),
                path: traversed,
                identity,
            });
        }
        parent = directory;
    }
    Err(AppPathsError::UnsafePath {
        path: path.to_path_buf(),
    })
}

fn open_private_child_directory(
    parent: &File,
    name: &OsStr,
    path: &Path,
) -> Result<PrivateDirectory, AppPathsError> {
    let directory =
        open_directory_at(parent, name).map_err(|source| AppPathsError::InspectPath {
            path: path.to_path_buf(),
            source,
        })?;
    set_file_mode(&directory, PRIVATE_DIRECTORY_MODE).map_err(|source| {
        AppPathsError::SetPermissions {
            path: path.to_path_buf(),
            source,
        }
    })?;
    let identity = validate_private_open_directory(&directory, path)?;
    let parent = parent
        .try_clone()
        .map_err(|source| AppPathsError::InspectPath {
            path: path.to_path_buf(),
            source,
        })?;
    Ok(PrivateDirectory {
        parent,
        file: directory,
        name: name.to_os_string(),
        path: path.to_path_buf(),
        identity,
    })
}

fn validate_private_open_directory(
    directory: &File,
    path: &Path,
) -> Result<DirectoryIdentity, AppPathsError> {
    let metadata = directory
        .metadata()
        .map_err(|source| AppPathsError::InspectPath {
            path: path.to_path_buf(),
            source,
        })?;
    if metadata.is_dir()
        && metadata.uid() == effective_user_id()
        && metadata.mode() & 0o777 == PRIVATE_DIRECTORY_MODE
    {
        Ok(DirectoryIdentity::from_metadata(&metadata))
    } else {
        Err(AppPathsError::UnsafePath {
            path: path.to_path_buf(),
        })
    }
}

fn component_cstring(name: &OsStr) -> io::Result<CString> {
    CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path component contains NUL"))
}

fn open_directory_at(parent: &File, name: &OsStr) -> io::Result<File> {
    let name = component_cstring(name)?;
    // SAFETY: `name` is NUL-terminated and the returned descriptor is checked before ownership.
    let descriptor = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if descriptor < 0 {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: `openat` returned a new owned descriptor.
        Ok(unsafe { File::from_raw_fd(descriptor) })
    }
}

fn create_directory_at(parent: &File, name: &OsStr) -> io::Result<()> {
    let name = component_cstring(name)?;
    // SAFETY: `name` is NUL-terminated and `mkdirat` does not retain the pointer.
    let result = unsafe {
        libc::mkdirat(
            parent.as_raw_fd(),
            name.as_ptr(),
            PRIVATE_DIRECTORY_MODE as libc::mode_t,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(test)]
fn create_file_at(parent: &File, name: &OsStr, path: &Path) -> Result<File, AppPathsError> {
    let name = component_cstring(name).map_err(|source| AppPathsError::CreateArtifact {
        path: path.to_path_buf(),
        source,
    })?;
    // SAFETY: `name` is NUL-terminated and the returned descriptor is checked before ownership.
    let descriptor = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            PRIVATE_ARTIFACT_MODE as libc::c_uint,
        )
    };
    if descriptor < 0 {
        Err(AppPathsError::CreateArtifact {
            path: path.to_path_buf(),
            source: io::Error::last_os_error(),
        })
    } else {
        // SAFETY: `openat` returned a new owned descriptor.
        Ok(unsafe { File::from_raw_fd(descriptor) })
    }
}

fn set_file_mode(file: &File, mode: u32) -> io::Result<()> {
    // SAFETY: `file` owns a valid descriptor and `fchmod` has no pointer arguments.
    let result = unsafe { libc::fchmod(file.as_raw_fd(), mode as libc::mode_t) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn inspect_socket_entry_at(
    parent: &File,
    name: &OsStr,
    path: &Path,
    expected_mode: Option<u32>,
) -> Result<DirectoryIdentity, AppPathsError> {
    let name = component_cstring(name).map_err(|source| AppPathsError::InspectPath {
        path: path.to_path_buf(),
        source,
    })?;
    // SAFETY: `status` is initialized for `fstatat`, and `name` is NUL-terminated.
    let mut status = unsafe { std::mem::zeroed::<libc::stat>() };
    // SAFETY: both pointers are valid for the duration of this call.
    let result = unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            &mut status,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result != 0 {
        return Err(AppPathsError::InspectPath {
            path: path.to_path_buf(),
            source: io::Error::last_os_error(),
        });
    }
    let mode = u32::from(status.st_mode);
    let socket = mode & u32::from(libc::S_IFMT) == u32::from(libc::S_IFSOCK);
    let expected_mode = expected_mode.is_none_or(|expected| mode & 0o777 == expected);
    if !socket || status.st_uid != effective_user_id() || !expected_mode {
        return Err(AppPathsError::UnsafePath {
            path: path.to_path_buf(),
        });
    }
    Ok(DirectoryIdentity {
        device: status.st_dev as u64,
        inode: status.st_ino,
    })
}

fn set_entry_mode_at(parent: &File, name: &OsStr, mode: u32) -> io::Result<()> {
    let name = component_cstring(name)?;
    // SAFETY: `name` is NUL-terminated and `fchmodat` does not retain the pointer.
    let result = unsafe {
        libc::fchmodat(
            parent.as_raw_fd(),
            name.as_ptr(),
            mode as libc::mode_t,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn remove_file_at(parent: &File, name: &OsStr) -> io::Result<()> {
    unlink_at(parent, name, 0)
}

fn remove_directory_at(parent: &File, name: &OsStr) -> io::Result<()> {
    unlink_at(parent, name, libc::AT_REMOVEDIR)
}

fn unlink_at(parent: &File, name: &OsStr, flags: i32) -> io::Result<()> {
    let name = component_cstring(name)?;
    // SAFETY: `name` is NUL-terminated and `unlinkat` does not retain the pointer.
    let result = unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), flags) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
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

fn effective_user_id() -> u32 {
    // SAFETY: geteuid has no pointer preconditions or side effects.
    unsafe { libc::geteuid() }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt, symlink};
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory {
        path: PathBuf,
    }

    impl TestDirectory {
        fn new(name: &str) -> Self {
            let sequence = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = Path::new("/private/tmp")
                .join(format!("stap-{}-{sequence}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn environment(root: &Path) -> AppPathEnvironment {
        AppPathEnvironment {
            home: Some(root.join("home").into_os_string()),
            xdg_config_home: None,
            xdg_data_home: None,
            xdg_state_home: None,
            xdg_cache_home: None,
            xdg_runtime_dir: None,
            macos_temporary_directory: root.join("temporary"),
        }
    }

    #[derive(Default)]
    struct TestAppPathEnvironmentReader {
        environment: BTreeMap<OsString, OsString>,
        environment_reads: BTreeMap<OsString, usize>,
        macos_temporary_directory: PathBuf,
        macos_temporary_directory_reads: usize,
    }

    impl TestAppPathEnvironmentReader {
        fn with_environment(mut self, key: &str, value: impl Into<OsString>) -> Self {
            self.environment.insert(key.into(), value.into());
            self
        }

        fn with_macos_temporary_directory(mut self, path: impl Into<PathBuf>) -> Self {
            self.macos_temporary_directory = path.into();
            self
        }

        fn environment_read_count(&self, key: &str) -> usize {
            self.environment_reads
                .get(OsStr::new(key))
                .copied()
                .unwrap_or_default()
        }
    }

    impl AppPathEnvironmentReader for TestAppPathEnvironmentReader {
        fn environment_variable(&mut self, key: &OsStr) -> Option<OsString> {
            *self
                .environment_reads
                .entry(key.to_os_string())
                .or_default() += 1;
            self.environment.get(key).cloned()
        }

        fn macos_temporary_directory(&mut self) -> PathBuf {
            self.macos_temporary_directory_reads += 1;
            self.macos_temporary_directory.clone()
        }
    }

    #[test]
    fn capture_should_read_each_input_exactly_once() {
        let mut reader = TestAppPathEnvironmentReader::default()
            .with_environment("HOME", "/Users/capture")
            .with_environment("XDG_CONFIG_HOME", "/capture/config")
            .with_environment("XDG_DATA_HOME", "/capture/data")
            .with_environment("XDG_STATE_HOME", "/capture/state")
            .with_environment("XDG_CACHE_HOME", "/capture/cache")
            .with_environment("XDG_RUNTIME_DIR", "/capture/runtime")
            .with_macos_temporary_directory("/capture/temporary");

        let captured = AppPathEnvironment::capture_with(&mut reader);

        assert_eq!(captured.home.as_deref(), Some(OsStr::new("/Users/capture")));
        assert_eq!(
            captured.xdg_config_home.as_deref(),
            Some(OsStr::new("/capture/config"))
        );
        assert_eq!(
            captured.xdg_data_home.as_deref(),
            Some(OsStr::new("/capture/data"))
        );
        assert_eq!(
            captured.xdg_state_home.as_deref(),
            Some(OsStr::new("/capture/state"))
        );
        assert_eq!(
            captured.xdg_cache_home.as_deref(),
            Some(OsStr::new("/capture/cache"))
        );
        assert_eq!(
            captured.xdg_runtime_dir.as_deref(),
            Some(OsStr::new("/capture/runtime"))
        );
        assert_eq!(
            captured.macos_temporary_directory,
            Path::new("/capture/temporary")
        );
        for key in [
            "HOME",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_STATE_HOME",
            "XDG_CACHE_HOME",
            "XDG_RUNTIME_DIR",
        ] {
            assert_eq!(
                reader.environment_read_count(key),
                1,
                "read count for {key}"
            );
        }
        assert_eq!(reader.macos_temporary_directory_reads, 1);
    }

    #[test]
    fn capture_should_remain_immutable_after_the_source_changes() {
        let mut reader = TestAppPathEnvironmentReader::default()
            .with_environment("HOME", "/Users/original")
            .with_environment("XDG_CONFIG_HOME", "/original/config")
            .with_macos_temporary_directory("/original/temporary");
        let captured = AppPathEnvironment::capture_with(&mut reader);

        reader
            .environment
            .insert("HOME".into(), "/Users/replacement".into());
        reader
            .environment
            .insert("XDG_CONFIG_HOME".into(), "/replacement/config".into());
        reader.macos_temporary_directory = PathBuf::from("/replacement/temporary");

        assert_eq!(
            captured.home.as_deref(),
            Some(OsStr::new("/Users/original"))
        );
        assert_eq!(
            captured.xdg_config_home.as_deref(),
            Some(OsStr::new("/original/config"))
        );
        assert_eq!(
            captured.macos_temporary_directory,
            Path::new("/original/temporary")
        );
    }

    #[test]
    fn captured_absent_and_invalid_values_should_use_existing_fallback_policy() {
        let mut reader = TestAppPathEnvironmentReader::default()
            .with_environment("HOME", "/Users/fallback")
            .with_environment("XDG_DATA_HOME", "")
            .with_environment("XDG_STATE_HOME", "relative-state")
            .with_environment("XDG_RUNTIME_DIR", "relative-runtime")
            .with_macos_temporary_directory("/private/tmp/captured");

        let captured = AppPathEnvironment::capture_with(&mut reader);
        let paths = AppPaths::resolve(&captured).unwrap();

        assert_eq!(
            paths.config(),
            Path::new("/Users/fallback/.config/spaceterm")
        );
        assert_eq!(
            paths.data(),
            Path::new("/Users/fallback/.local/share/spaceterm")
        );
        assert_eq!(
            paths.state(),
            Path::new("/Users/fallback/.local/state/spaceterm")
        );
        assert_eq!(paths.cache(), Path::new("/Users/fallback/.cache/spaceterm"));
        assert_eq!(
            paths.runtime(),
            Path::new("/private/tmp/captured/spaceterm")
        );
    }

    #[test]
    fn captured_relative_temporary_directory_should_remain_invalid() {
        let mut reader = TestAppPathEnvironmentReader::default()
            .with_environment("HOME", "/Users/fallback")
            .with_macos_temporary_directory("relative-temporary");

        let captured = AppPathEnvironment::capture_with(&mut reader);
        let error = AppPaths::resolve(&captured).unwrap_err();

        assert!(matches!(error, AppPathsError::InvalidTemporaryDirectory));
    }

    #[test]
    fn capture_should_preserve_non_utf8_paths() {
        let non_utf8_home = OsString::from_vec(b"/Users/home-\xff".to_vec());
        let non_utf8_config = OsString::from_vec(b"/private/config-\xfe".to_vec());
        let non_utf8_temporary = OsString::from_vec(b"/private/tmp/temp-\xfd".to_vec());
        let mut reader = TestAppPathEnvironmentReader::default()
            .with_environment("HOME", non_utf8_home.clone())
            .with_environment("XDG_CONFIG_HOME", non_utf8_config.clone())
            .with_macos_temporary_directory(PathBuf::from(non_utf8_temporary.clone()));

        let captured = AppPathEnvironment::capture_with(&mut reader);

        assert_eq!(
            captured.home.as_deref().unwrap().as_bytes(),
            non_utf8_home.as_bytes()
        );
        assert_eq!(
            captured.xdg_config_home.as_deref().unwrap().as_bytes(),
            non_utf8_config.as_bytes()
        );
        assert_eq!(
            captured.macos_temporary_directory.as_os_str().as_bytes(),
            non_utf8_temporary.as_bytes()
        );
    }

    #[test]
    fn resolve_should_use_valid_absolute_xdg_roots() {
        let root = TestDirectory::new("absolute-xdg");
        let environment = AppPathEnvironment {
            home: None,
            xdg_config_home: Some(root.path.join("config").into_os_string()),
            xdg_data_home: Some(root.path.join("data").into_os_string()),
            xdg_state_home: Some(root.path.join("state").into_os_string()),
            xdg_cache_home: Some(root.path.join("cache").into_os_string()),
            xdg_runtime_dir: Some(root.path.join("runtime").into_os_string()),
            macos_temporary_directory: root.path.join("temporary"),
        };

        let paths = AppPaths::resolve(&environment).unwrap();

        assert_eq!(paths.config(), root.path.join("config/spaceterm"));
        assert_eq!(paths.data(), root.path.join("data/spaceterm"));
        assert_eq!(paths.state(), root.path.join("state/spaceterm"));
        assert_eq!(paths.cache(), root.path.join("cache/spaceterm"));
        assert_eq!(paths.runtime(), root.path.join("runtime/spaceterm"));
    }

    #[test]
    fn resolve_should_fall_back_for_unset_empty_and_relative_xdg_values() {
        let root = TestDirectory::new("fallbacks");
        let mut environment = environment(&root.path);
        environment.xdg_data_home = Some(OsString::new());
        environment.xdg_state_home = Some(OsString::from("relative-state"));
        environment.xdg_cache_home = Some(OsString::from(""));
        environment.xdg_runtime_dir = Some(OsString::from("relative-runtime"));

        let paths = AppPaths::resolve(&environment).unwrap();

        assert_eq!(paths.config(), root.path.join("home/.config/spaceterm"));
        assert_eq!(paths.data(), root.path.join("home/.local/share/spaceterm"));
        assert_eq!(paths.state(), root.path.join("home/.local/state/spaceterm"));
        assert_eq!(paths.cache(), root.path.join("home/.cache/spaceterm"));
        assert_eq!(paths.runtime(), root.path.join("temporary/spaceterm"));
    }

    #[test]
    fn macos_fallback_runtime_should_resolve_the_system_style_var_symlink_once() {
        let root = TestDirectory::new("macos-var-runtime");
        let canonical_temporary = root.path.join("private/var/folders/user/T");
        fs::create_dir_all(&canonical_temporary).unwrap();
        fs::set_permissions(&canonical_temporary, fs::Permissions::from_mode(0o755)).unwrap();
        symlink(root.path.join("private/var"), root.path.join("var")).unwrap();
        let captured_temporary =
            canonicalize_macos_temporary_directory(root.path.join("var/folders/user/T"));
        let paths = AppPaths::resolve(&AppPathEnvironment {
            home: Some(root.path.join("home").into_os_string()),
            macos_temporary_directory: captured_temporary.clone(),
            ..AppPathEnvironment::default()
        })
        .unwrap();

        let owner = paths.create_runtime_owner("askpass").unwrap();

        assert!(
            captured_temporary == canonical_temporary
                && paths.runtime() == canonical_temporary.join("spaceterm")
                && fs::metadata(&canonical_temporary).unwrap().mode() & 0o777 == 0o755
                && fs::metadata(paths.runtime()).unwrap().mode() & 0o777 == 0o700
                && fs::metadata(owner.path()).unwrap().mode() & 0o777 == 0o700
        );
    }

    #[test]
    fn explicit_xdg_runtime_should_still_reject_a_symlinked_base() {
        let root = TestDirectory::new("xdg-runtime-symlink");
        let outside = root.path.join("outside-runtime");
        fs::create_dir_all(&outside).unwrap();
        let linked_runtime = root.path.join("xdg-runtime");
        symlink(&outside, &linked_runtime).unwrap();
        let paths = AppPaths::resolve(&AppPathEnvironment {
            home: Some(root.path.join("home").into_os_string()),
            xdg_runtime_dir: Some(linked_runtime.into_os_string()),
            macos_temporary_directory: root.path.join("temporary"),
            ..AppPathEnvironment::default()
        })
        .unwrap();

        let Err(error) = paths.create_runtime_owner("askpass") else {
            panic!("the symlinked XDG runtime base must remain unavailable");
        };

        assert!(
            matches!(error, AppPathsError::UnsafePath { .. })
                && !outside.join("spaceterm").exists()
        );
    }

    #[test]
    fn resolve_should_require_home_only_when_an_xdg_fallback_needs_it() {
        let root = TestDirectory::new("missing-home");
        let mut complete = AppPathEnvironment {
            home: None,
            xdg_config_home: Some(root.path.join("config").into_os_string()),
            xdg_data_home: Some(root.path.join("data").into_os_string()),
            xdg_state_home: Some(root.path.join("state").into_os_string()),
            xdg_cache_home: Some(root.path.join("cache").into_os_string()),
            xdg_runtime_dir: None,
            macos_temporary_directory: root.path.join("temporary"),
        };
        assert!(AppPaths::resolve(&complete).is_ok());

        complete.xdg_config_home = None;
        let error = AppPaths::resolve(&complete).unwrap_err();

        assert!(matches!(
            error,
            AppPathsError::MissingHome {
                root: AppPathRoot::Config
            }
        ));
    }

    #[test]
    fn managed_ssh_config_should_live_under_the_config_root() {
        let root = TestDirectory::new("ssh-config");
        let paths = AppPaths::resolve(&environment(&root.path)).unwrap();

        assert_eq!(
            paths.managed_ssh_config(),
            root.path.join("home/.config/spaceterm/ssh_config")
        );
    }

    #[test]
    fn ensure_root_should_create_only_the_requested_root_with_mode_0700() {
        let root = TestDirectory::new("lazy-root");
        let paths = AppPaths::resolve(&environment(&root.path)).unwrap();

        let config = paths.ensure_root(AppPathRoot::Config).unwrap();

        assert!(config.is_dir());
        assert!(!paths.data().exists());
        assert_eq!(fs::metadata(config).unwrap().mode() & 0o777, 0o700);
    }

    #[test]
    fn ensure_root_should_tighten_an_existing_application_root() {
        let root = TestDirectory::new("tighten-root");
        let paths = AppPaths::resolve(&environment(&root.path)).unwrap();
        fs::create_dir_all(paths.state()).unwrap();
        fs::set_permissions(paths.state(), fs::Permissions::from_mode(0o755)).unwrap();

        paths.ensure_root(AppPathRoot::State).unwrap();

        assert_eq!(fs::metadata(paths.state()).unwrap().mode() & 0o777, 0o700);
    }

    #[test]
    fn ensure_root_should_create_each_missing_owned_component_restrictively() {
        let root = TestDirectory::new("restrictive-components");
        let paths = AppPaths::resolve(&environment(&root.path)).unwrap();

        paths.ensure_root(AppPathRoot::Config).unwrap();

        for path in [
            root.path.join("home"),
            root.path.join("home/.config"),
            root.path.join("home/.config/spaceterm"),
        ] {
            assert_eq!(
                fs::symlink_metadata(&path).unwrap().mode() & 0o777,
                0o700,
                "{} was not private from creation",
                path.display()
            );
        }
    }

    #[test]
    fn ensure_root_should_reject_a_symlinked_intermediate_component() {
        let root = TestDirectory::new("symlink-component");
        let outside = root.path.join("outside");
        fs::create_dir_all(&outside).unwrap();
        symlink(&outside, root.path.join("home")).unwrap();
        let paths = AppPaths::resolve(&environment(&root.path)).unwrap();

        let error = paths.ensure_root(AppPathRoot::Config).unwrap_err();

        assert!(matches!(error, AppPathsError::UnsafePath { .. }));
        assert!(!outside.join(".config/spaceterm").exists());
    }

    #[test]
    fn runtime_owner_should_be_unique_private_and_remove_only_itself_on_close() {
        let root = TestDirectory::new("runtime-owner");
        let paths = AppPaths::resolve(&environment(&root.path)).unwrap();
        let sibling = paths.runtime().join("keep");
        fs::create_dir_all(&sibling).unwrap();
        let first = paths.create_runtime_owner("ssh").unwrap();
        let second = paths.create_runtime_owner("ssh").unwrap();
        let first_path = first.path().to_path_buf();

        assert_ne!(first.path(), second.path());
        assert_eq!(fs::metadata(first.path()).unwrap().mode() & 0o777, 0o700);
        first.close().unwrap();

        assert!(!first_path.exists());
        assert!(second.path().exists());
        assert!(sibling.exists());
    }

    #[test]
    fn runtime_owner_drop_should_remove_its_exact_directory() {
        let root = TestDirectory::new("runtime-owner-drop");
        let paths = AppPaths::resolve(&environment(&root.path)).unwrap();
        let owner = paths.create_runtime_owner("askpass").unwrap();
        let owner_path = owner.path().to_path_buf();

        drop(owner);

        assert!(!owner_path.exists());
        assert!(paths.runtime().exists());
    }

    #[test]
    fn runtime_owner_should_reject_a_replaced_path_before_returning_a_socket_path() {
        let root = TestDirectory::new("replaced-owner");
        let paths = AppPaths::resolve(&environment(&root.path)).unwrap();
        let owner = paths.create_runtime_owner("ssh").unwrap();
        let original = owner.path().to_path_buf();
        let moved = paths.runtime().join("moved-owner");
        let outside = root.path.join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::rename(&original, &moved).unwrap();
        symlink(&outside, &original).unwrap();

        let error = owner.socket_path("control.sock").unwrap_err();

        assert!(matches!(error, AppPathsError::UnsafePath { .. }));
        drop(owner);
        assert!(outside.exists());
        fs::remove_file(original).unwrap();
        fs::remove_dir(moved).unwrap();
    }

    #[test]
    fn runtime_owner_close_should_not_recursively_delete_untracked_contents() {
        let root = TestDirectory::new("untracked-owner-content");
        let paths = AppPaths::resolve(&environment(&root.path)).unwrap();
        let owner = paths.create_runtime_owner("ssh").unwrap();
        let owner_path = owner.path().to_path_buf();
        fs::write(owner_path.join("not-owned-by-api"), b"keep").unwrap();

        let error = owner.close().unwrap_err();

        assert!(matches!(error, AppPathsError::Cleanup { .. }));
        assert!(owner_path.join("not-owned-by-api").exists());
    }

    #[test]
    fn runtime_artifact_should_be_owner_only_and_reject_path_traversal() {
        let root = TestDirectory::new("runtime-artifact");
        let paths = AppPaths::resolve(&environment(&root.path)).unwrap();
        let owner = paths.create_runtime_owner("ssh").unwrap();

        let artifact = owner.create_artifact("status").unwrap();

        assert_eq!(artifact.path(), owner.path().join("status"));
        assert_eq!(artifact.file().metadata().unwrap().mode() & 0o777, 0o600);
        assert!(matches!(
            owner.create_artifact("../outside"),
            Err(AppPathsError::InvalidArtifactName)
        ));
    }

    #[test]
    fn socket_path_should_accept_the_macos_limit_and_reject_one_byte_more() {
        let root = TestDirectory::new("socket-length");
        let paths = AppPaths::resolve(&environment(&root.path)).unwrap();
        let owner = paths.create_runtime_owner("s").unwrap();
        let prefix_bytes = owner.path().as_os_str().as_bytes().len() + 1;
        let available = MACOS_UNIX_SOCKET_PATH_BYTES - prefix_bytes;
        let accepted = "s".repeat(available);
        let rejected = "s".repeat(available + 1);

        let socket = owner.socket_path(&accepted).unwrap();

        assert_eq!(
            socket.as_os_str().as_bytes().len(),
            MACOS_UNIX_SOCKET_PATH_BYTES
        );
        assert!(matches!(
            owner.socket_path(&rejected),
            Err(AppPathsError::SocketPathTooLong {
                actual,
                maximum: MACOS_UNIX_SOCKET_PATH_BYTES,
            }) if actual == MACOS_UNIX_SOCKET_PATH_BYTES + 1
        ));
    }

    #[test]
    fn registered_socket_should_be_owner_only_and_removed_with_its_runtime_owner() {
        let root = TestDirectory::new("registered-socket");
        let paths = AppPaths::resolve(&environment(&root.path)).unwrap();
        let owner = paths.create_runtime_owner("ssh").unwrap();
        let socket_path = owner.socket_path("control.sock").unwrap();
        let listener = UnixListener::bind(&socket_path).unwrap();

        let socket = owner.register_socket("control.sock").unwrap();

        let metadata = fs::symlink_metadata(socket.path()).unwrap();
        assert!(metadata.file_type().is_socket());
        assert_eq!(metadata.uid(), unsafe { libc::geteuid() });
        assert_eq!(metadata.mode() & 0o777, 0o600);
        drop(listener);
        owner.close().unwrap();
        assert!(!socket_path.exists());
    }

    #[test]
    fn socket_registration_should_reject_a_non_socket_artifact() {
        let root = TestDirectory::new("invalid-socket");
        let paths = AppPaths::resolve(&environment(&root.path)).unwrap();
        let owner = paths.create_runtime_owner("ssh").unwrap();
        let socket_path = owner.socket_path("control.sock").unwrap();
        fs::write(&socket_path, b"not a socket").unwrap();

        let error = owner.register_socket("control.sock").unwrap_err();

        assert!(matches!(error, AppPathsError::UnsafePath { .. }));
        fs::remove_file(socket_path).unwrap();
        owner.close().unwrap();
    }

    #[test]
    fn registered_socket_should_reject_replacement_and_owner_cleanup_should_not_unlink_it() {
        let root = TestDirectory::new("replaced-socket");
        let paths = AppPaths::resolve(&environment(&root.path)).unwrap();
        let owner = paths.create_runtime_owner("ssh").unwrap();
        let socket_path = owner.socket_path("control.sock").unwrap();
        let listener = UnixListener::bind(&socket_path).unwrap();
        let socket = owner.register_socket("control.sock").unwrap();
        drop(listener);
        fs::remove_file(&socket_path).unwrap();
        let replacement = UnixListener::bind(&socket_path).unwrap();

        let verification = socket.verify();
        let cleanup = owner.close();

        assert!(
            matches!(verification, Err(AppPathsError::UnsafePath { .. }))
                && matches!(cleanup, Err(AppPathsError::UnsafePath { .. }))
                && socket_path.exists()
        );
        drop(replacement);
        fs::remove_file(socket_path).unwrap();
    }

    #[test]
    fn created_directories_and_artifacts_should_belong_to_the_effective_user() {
        let root = TestDirectory::new("ownership");
        let paths = AppPaths::resolve(&environment(&root.path)).unwrap();
        let owner = paths.create_runtime_owner("ssh").unwrap();
        let artifact = owner.create_artifact("owner-check").unwrap();

        assert_eq!(
            (
                fs::metadata(paths.runtime()).unwrap().uid(),
                fs::metadata(owner.path()).unwrap().uid(),
                artifact.file().metadata().unwrap().uid(),
            ),
            {
                // SAFETY: geteuid has no pointer preconditions or side effects.
                let owner = unsafe { libc::geteuid() };
                (owner, owner, owner)
            }
        );
    }
}
