use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use thiserror::Error;

const MACOS_UNIX_SOCKET_PATH_BYTES: usize = 103;
const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
const PRIVATE_ARTIFACT_MODE: u32 = 0o600;
const RUNTIME_OWNER_CREATION_ATTEMPTS: usize = 128;

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AppPathRoot {
    Config,
    Data,
    State,
    Cache,
    Runtime,
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

    pub(crate) fn data(&self) -> &Path {
        &self.data
    }

    pub(crate) fn state(&self) -> &Path {
        &self.state
    }

    pub(crate) fn cache(&self) -> &Path {
        &self.cache
    }

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
        let runtime_root = self.ensure_root(AppPathRoot::Runtime)?;
        for _ in 0..RUNTIME_OWNER_CREATION_ATTEMPTS {
            let sequence = NEXT_RUNTIME_OWNER.fetch_add(1, Ordering::Relaxed);
            let name = format!("{kind}-{}-{sequence:016x}", std::process::id());
            let path = runtime_root.join(name);
            let mut builder = fs::DirBuilder::new();
            builder.mode(PRIVATE_DIRECTORY_MODE);
            match builder.create(&path) {
                Ok(()) => {
                    fs::set_permissions(&path, fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE))
                        .map_err(|source| AppPathsError::SetPermissions {
                            path: path.clone(),
                            source,
                        })?;
                    let identity = private_directory_identity(&path)?;
                    return Ok(RuntimeOwner {
                        runtime_root: runtime_root.to_path_buf(),
                        path,
                        identity,
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
            AppPathRoot::Runtime => &self.runtime,
        }
    }
}

pub(crate) struct RuntimeOwner {
    runtime_root: PathBuf,
    path: PathBuf,
    identity: DirectoryIdentity,
    closed: bool,
}

impl RuntimeOwner {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn socket_path(&self, name: &str) -> Result<PathBuf, AppPathsError> {
        validate_child_name(name)?;
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

    pub(crate) fn create_artifact(&self, name: &str) -> Result<RuntimeArtifact, AppPathsError> {
        validate_child_name(name)?;
        self.verify_identity()?;
        let path = self.path.join(name);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(PRIVATE_ARTIFACT_MODE)
            .open(&path)
            .map_err(|source| AppPathsError::CreateArtifact {
                path: path.clone(),
                source,
            })?;
        fs::set_permissions(&path, fs::Permissions::from_mode(PRIVATE_ARTIFACT_MODE)).map_err(
            |source| AppPathsError::SetPermissions {
                path: path.clone(),
                source,
            },
        )?;
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
            drop(file);
            let _ = fs::remove_file(&path);
            return Err(AppPathsError::UnsafePath { path });
        }
        Ok(RuntimeArtifact { path, file })
    }

    pub(crate) fn close(mut self) -> Result<(), AppPathsError> {
        let result = self.cleanup();
        self.closed = true;
        result
    }

    fn cleanup(&self) -> Result<(), AppPathsError> {
        self.verify_identity()?;
        match fs::remove_dir_all(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(AppPathsError::Cleanup {
                path: self.path.clone(),
                source,
            }),
        }
    }

    fn verify_identity(&self) -> Result<(), AppPathsError> {
        if self.path.parent() != Some(self.runtime_root.as_path()) {
            return Err(AppPathsError::UnsafePath {
                path: self.path.clone(),
            });
        }
        match fs::symlink_metadata(&self.path) {
            Ok(metadata)
                if metadata.file_type().is_dir()
                    && !metadata.file_type().is_symlink()
                    && metadata.uid() == effective_user_id()
                    && DirectoryIdentity::from_metadata(&metadata) == self.identity =>
            {
                Ok(())
            }
            Ok(_) => Err(AppPathsError::UnsafePath {
                path: self.path.clone(),
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(AppPathsError::InspectPath {
                path: self.path.clone(),
                source,
            }),
        }
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

pub(crate) struct RuntimeArtifact {
    path: PathBuf,
    file: File,
}

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

fn ensure_private_directory(path: &Path) -> Result<DirectoryIdentity, AppPathsError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => validate_private_directory(path, &metadata)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(path).map_err(|source| AppPathsError::CreateDirectory {
                path: path.to_path_buf(),
                source,
            })?;
        }
        Err(source) => {
            return Err(AppPathsError::InspectPath {
                path: path.to_path_buf(),
                source,
            });
        }
    }
    let metadata = fs::symlink_metadata(path).map_err(|source| AppPathsError::InspectPath {
        path: path.to_path_buf(),
        source,
    })?;
    validate_private_directory(path, &metadata)?;
    fs::set_permissions(path, fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE)).map_err(
        |source| AppPathsError::SetPermissions {
            path: path.to_path_buf(),
            source,
        },
    )?;
    private_directory_identity(path)
}

fn validate_private_directory(path: &Path, metadata: &fs::Metadata) -> Result<(), AppPathsError> {
    if metadata.file_type().is_dir()
        && !metadata.file_type().is_symlink()
        && metadata.uid() == effective_user_id()
    {
        Ok(())
    } else {
        Err(AppPathsError::UnsafePath {
            path: path.to_path_buf(),
        })
    }
}

fn private_directory_identity(path: &Path) -> Result<DirectoryIdentity, AppPathsError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| AppPathsError::InspectPath {
        path: path.to_path_buf(),
        source,
    })?;
    validate_private_directory(path, &metadata)?;
    if metadata.mode() & 0o777 != PRIVATE_DIRECTORY_MODE {
        return Err(AppPathsError::UnsafePath {
            path: path.to_path_buf(),
        });
    }
    Ok(DirectoryIdentity::from_metadata(&metadata))
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
    use std::fs;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory {
        path: PathBuf,
    }

    impl TestDirectory {
        fn new(name: &str) -> Self {
            let sequence = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path =
                Path::new("/tmp").join(format!("stap-{}-{sequence}-{name}", std::process::id()));
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
