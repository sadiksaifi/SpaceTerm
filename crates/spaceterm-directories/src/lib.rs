//! Resolves SpaceTerm's semantic application directories without creating them.

use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf};

use thiserror::Error;

pub const APP_DIR_NAME: &str = "spaceterm";
const WINDOWS_VENDOR_DIR_NAME: &str = "sadiksaifi";
const SETTINGS_DOCUMENT_NAME: &str = "settings.json";
const MANAGED_SSH_CONFIG_NAME: &str = "ssh_config";

const HOME_ENVIRONMENT_VARIABLE: &str = "HOME";
const TMPDIR_ENVIRONMENT_VARIABLE: &str = "TMPDIR";
const XDG_CONFIG_HOME_ENVIRONMENT_VARIABLE: &str = "XDG_CONFIG_HOME";
const XDG_DATA_HOME_ENVIRONMENT_VARIABLE: &str = "XDG_DATA_HOME";
const XDG_STATE_HOME_ENVIRONMENT_VARIABLE: &str = "XDG_STATE_HOME";
const XDG_CACHE_HOME_ENVIRONMENT_VARIABLE: &str = "XDG_CACHE_HOME";
const XDG_RUNTIME_DIR_ENVIRONMENT_VARIABLE: &str = "XDG_RUNTIME_DIR";

#[derive(Clone, Default, Eq, PartialEq)]
pub struct AppDirectoryEnvironment {
    pub home: Option<OsString>,
    pub tmpdir: Option<OsString>,
    pub xdg_config_home: Option<OsString>,
    pub xdg_data_home: Option<OsString>,
    pub xdg_state_home: Option<OsString>,
    pub xdg_cache_home: Option<OsString>,
    pub xdg_runtime_dir: Option<OsString>,
}

impl std::fmt::Debug for AppDirectoryEnvironment {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("AppDirectoryEnvironment(<redacted>)")
    }
}

impl AppDirectoryEnvironment {
    pub fn capture() -> Self {
        Self {
            home: std::env::var_os(HOME_ENVIRONMENT_VARIABLE),
            tmpdir: std::env::var_os(TMPDIR_ENVIRONMENT_VARIABLE),
            xdg_config_home: std::env::var_os(XDG_CONFIG_HOME_ENVIRONMENT_VARIABLE),
            xdg_data_home: std::env::var_os(XDG_DATA_HOME_ENVIRONMENT_VARIABLE),
            xdg_state_home: std::env::var_os(XDG_STATE_HOME_ENVIRONMENT_VARIABLE),
            xdg_cache_home: std::env::var_os(XDG_CACHE_HOME_ENVIRONMENT_VARIABLE),
            xdg_runtime_dir: std::env::var_os(XDG_RUNTIME_DIR_ENVIRONMENT_VARIABLE),
        }
    }

    pub fn configured_runtime_root(&self) -> Option<PathBuf> {
        absolute_environment_path(self.xdg_runtime_dir.as_deref())
    }

    pub fn configured_temporary_root(&self) -> Option<PathBuf> {
        absolute_environment_path(self.tmpdir.as_deref())
            .filter(|path| is_absolute_normal_path(path))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AppDirectoryRoot {
    Config,
    Data,
    State,
    Cache,
}

/// Semantic storage locations shared by every platform adapter.
///
/// Resolution is side-effect free. Callers create a directory only when they are about to write
/// through the application's secure filesystem authority.
#[derive(Clone, Eq, PartialEq)]
pub struct AppDirectories {
    pub config: PathBuf,
    pub data: PathBuf,
    pub state: PathBuf,
    pub cache: PathBuf,
    pub runtime: Option<PathBuf>,
    logs: PathBuf,
    managed_ssh_config: PathBuf,
    temporary: Option<PathBuf>,
}

impl AppDirectories {
    /// Resolves the current platform's application directories without creating them.
    pub fn resolve(app_name: &str) -> Result<Self, DirectoryError> {
        validate_directory_name(app_name)?;

        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let environment = AppDirectoryEnvironment::capture();
            return Self::resolve_xdg_for_host(app_name, &environment, os_temporary_directory);
        }

        #[cfg(target_os = "windows")]
        {
            return windows::resolve(app_name);
        }

        #[allow(unreachable_code)]
        Err(DirectoryError::UnsupportedPlatform)
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn resolve_xdg_for_host(
        app_name: &str,
        environment: &AppDirectoryEnvironment,
        os_temporary: impl FnOnce() -> Option<PathBuf>,
    ) -> Result<Self, DirectoryError> {
        let temporary = environment
            .configured_temporary_root()
            .and_then(canonical_temporary_directory)
            .or_else(|| os_temporary().and_then(canonical_temporary_directory));
        Self::resolve_xdg(app_name, environment, temporary)
    }

    pub fn resolve_xdg(
        app_name: &str,
        environment: &AppDirectoryEnvironment,
        runtime_fallback: Option<PathBuf>,
    ) -> Result<Self, DirectoryError> {
        validate_directory_name(app_name)?;
        let home = absolute_environment_path(environment.home.as_deref());
        let config = resolve_xdg_root(
            environment.xdg_config_home.as_deref(),
            home.as_deref(),
            AppDirectoryRoot::Config,
            &[".config"],
            app_name,
        )?;
        let data = resolve_xdg_root(
            environment.xdg_data_home.as_deref(),
            home.as_deref(),
            AppDirectoryRoot::Data,
            &[".local", "share"],
            app_name,
        )?;
        let state = resolve_xdg_root(
            environment.xdg_state_home.as_deref(),
            home.as_deref(),
            AppDirectoryRoot::State,
            &[".local", "state"],
            app_name,
        )?;
        let cache = resolve_xdg_root(
            environment.xdg_cache_home.as_deref(),
            home.as_deref(),
            AppDirectoryRoot::Cache,
            &[".cache"],
            app_name,
        )?;
        let temporary = runtime_fallback.filter(|path| is_absolute_normal_path(path));
        let runtime = environment
            .configured_runtime_root()
            .or_else(|| temporary.clone())
            .map(|root| root.join(app_name));
        let logs = state.join("logs");
        let managed_ssh_config = config.join(MANAGED_SSH_CONFIG_NAME);
        Ok(Self {
            config,
            data,
            state,
            cache,
            runtime,
            logs,
            managed_ssh_config,
            temporary,
        })
    }

    pub fn resolve_windows(
        app_name: &str,
        roaming_app_data: PathBuf,
        local_app_data: PathBuf,
        temporary: Option<PathBuf>,
    ) -> Result<Self, DirectoryError> {
        validate_directory_name(app_name)?;
        validate_native_root(&roaming_app_data, NativeDirectoryRoot::RoamingAppData)?;
        validate_native_root(&local_app_data, NativeDirectoryRoot::LocalAppData)?;
        if let Some(temporary) = temporary.as_deref() {
            validate_native_root(temporary, NativeDirectoryRoot::Temporary)?;
        }

        let roaming_application = roaming_app_data
            .join(WINDOWS_VENDOR_DIR_NAME)
            .join(app_name);
        let local_application = local_app_data.join(WINDOWS_VENDOR_DIR_NAME).join(app_name);
        let data = local_application.join("Data");
        let runtime = temporary.as_ref().map(|root| {
            root.join(WINDOWS_VENDOR_DIR_NAME)
                .join(app_name)
                .join("Runtime")
        });
        Ok(Self {
            config: roaming_application,
            managed_ssh_config: data.join(MANAGED_SSH_CONFIG_NAME),
            data,
            state: local_application.join("State"),
            cache: local_application.join("Cache"),
            runtime,
            logs: local_application.join("Logs"),
            temporary,
        })
    }

    pub fn config_file(&self) -> PathBuf {
        self.config.join(SETTINGS_DOCUMENT_NAME)
    }

    pub fn managed_ssh_config(&self) -> PathBuf {
        self.managed_ssh_config.clone()
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.logs.clone()
    }

    pub fn session_file(&self) -> PathBuf {
        self.state.join("session.json")
    }

    pub fn window_state_file(&self) -> PathBuf {
        self.state.join("window-state.json")
    }

    pub fn temporary_directory(&self) -> Option<&Path> {
        self.temporary.as_deref()
    }

    pub fn root(&self, root: AppDirectoryRoot) -> &Path {
        match root {
            AppDirectoryRoot::Config => &self.config,
            AppDirectoryRoot::Data => &self.data,
            AppDirectoryRoot::State => &self.state,
            AppDirectoryRoot::Cache => &self.cache,
        }
    }
}

impl std::fmt::Debug for AppDirectories {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("AppDirectories(..)")
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum DirectoryError {
    #[error("the application directory name is invalid")]
    InvalidAppName,
    #[error("HOME is required to resolve the {root:?} application directory")]
    MissingHome { root: AppDirectoryRoot },
    #[error("the {root:?} native directory root is invalid")]
    InvalidNativeRoot { root: NativeDirectoryRoot },
    #[error("the operating system could not resolve application directories")]
    NativeResolutionUnavailable,
    #[error("application directories are unsupported on this platform")]
    UnsupportedPlatform,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeDirectoryRoot {
    RoamingAppData,
    LocalAppData,
    Temporary,
}

fn absolute_environment_path(value: Option<&OsStr>) -> Option<PathBuf> {
    value
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn os_temporary_directory() -> Option<PathBuf> {
    let path = std::env::temp_dir();
    if let Some(path) = canonical_temporary_directory(path) {
        return Some(path);
    }

    #[cfg(unix)]
    {
        canonical_temporary_directory(PathBuf::from("/tmp"))
    }

    #[cfg(not(unix))]
    {
        None
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn canonical_temporary_directory(path: PathBuf) -> Option<PathBuf> {
    is_absolute_normal_path(&path)
        .then(|| std::fs::canonicalize(path).ok())
        .flatten()
        .filter(|path| is_absolute_normal_path(path))
}

fn is_absolute_normal_path(path: &Path) -> bool {
    path.is_absolute()
        && path.components().all(|component| {
            matches!(
                component,
                Component::Prefix(_) | Component::RootDir | Component::Normal(_)
            )
        })
}

fn resolve_xdg_root(
    configured: Option<&OsStr>,
    home: Option<&Path>,
    root: AppDirectoryRoot,
    fallback_components: &[&str],
    app_name: &str,
) -> Result<PathBuf, DirectoryError> {
    let base = match absolute_environment_path(configured) {
        Some(configured) => configured,
        None => {
            let home = home.ok_or(DirectoryError::MissingHome { root })?;
            fallback_components
                .iter()
                .fold(home.to_path_buf(), |path, component| path.join(component))
        }
    };
    Ok(base.join(app_name))
}

fn validate_directory_name(name: &str) -> Result<(), DirectoryError> {
    let mut components = Path::new(name).components();
    let valid = !name.is_empty()
        && !name.as_bytes().contains(&0)
        && matches!(components.next(), Some(Component::Normal(_)))
        && components.next().is_none();
    if valid {
        Ok(())
    } else {
        Err(DirectoryError::InvalidAppName)
    }
}

fn validate_native_root(path: &Path, root: NativeDirectoryRoot) -> Result<(), DirectoryError> {
    if is_absolute_normal_path(path) {
        Ok(())
    } else {
        Err(DirectoryError::InvalidNativeRoot { root })
    }
}

#[cfg(target_os = "windows")]
mod windows {
    use std::os::windows::ffi::OsStringExt;
    use std::ptr;

    use windows_sys::Win32::Foundation::RPC_E_CHANGED_MODE;
    use windows_sys::Win32::Storage::FileSystem::GetTempPathW;
    use windows_sys::Win32::System::Com::{
        COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize,
    };
    use windows_sys::Win32::UI::Shell::{
        FOLDERID_LocalAppData, FOLDERID_RoamingAppData, SHGetKnownFolderPath,
    };
    use windows_sys::core::{GUID, PWSTR};

    use super::*;

    pub(super) fn resolve(app_name: &str) -> Result<AppDirectories, DirectoryError> {
        let _apartment = ComApartment::initialize()?;
        let roaming_app_data = known_folder(&FOLDERID_RoamingAppData)?;
        let local_app_data = known_folder(&FOLDERID_LocalAppData)?;
        AppDirectories::resolve_windows(
            app_name,
            roaming_app_data,
            local_app_data,
            temporary_directory(),
        )
    }

    fn known_folder(identifier: &GUID) -> Result<PathBuf, DirectoryError> {
        let mut value: PWSTR = ptr::null_mut();
        // SAFETY: COM is initialized above and `value` receives an optional COM allocation.
        let result = unsafe { SHGetKnownFolderPath(identifier, 0, ptr::null_mut(), &mut value) };
        let path = if result >= 0 && !value.is_null() {
            let mut length = 0;
            // SAFETY: successful `SHGetKnownFolderPath` returns a readable NUL-terminated allocation.
            while unsafe { *value.add(length) } != 0 {
                length += 1;
            }
            // SAFETY: the allocation contains at least `length` initialized UTF-16 code units.
            Some(PathBuf::from(OsString::from_wide(unsafe {
                std::slice::from_raw_parts(value, length)
            })))
        } else {
            None
        };
        // SAFETY: `value` was allocated by `SHGetKnownFolderPath` for `CoTaskMemFree`.
        unsafe { CoTaskMemFree(value.cast()) };
        path.ok_or(DirectoryError::NativeResolutionUnavailable)
    }

    fn temporary_directory() -> Option<PathBuf> {
        let mut buffer = vec![0_u16; 260];
        loop {
            // SAFETY: `buffer` is writable for the advertised number of UTF-16 code units.
            let length = unsafe { GetTempPathW(buffer.len() as u32, buffer.as_mut_ptr()) } as usize;
            if length == 0 {
                return None;
            }
            if length < buffer.len() {
                buffer.truncate(length);
                let path = PathBuf::from(OsString::from_wide(&buffer));
                return is_absolute_normal_path(&path).then_some(path);
            }
            buffer.resize(length.saturating_add(1), 0);
        }
    }

    struct ComApartment {
        owned: bool,
    }

    impl ComApartment {
        fn initialize() -> Result<Self, DirectoryError> {
            // SAFETY: the reserved pointer is null and successful initialization is balanced in Drop.
            let result = unsafe { CoInitializeEx(ptr::null(), COINIT_APARTMENTTHREADED as u32) };
            if result >= 0 {
                Ok(Self { owned: true })
            } else if result == RPC_E_CHANGED_MODE {
                Ok(Self { owned: false })
            } else {
                Err(DirectoryError::NativeResolutionUnavailable)
            }
        }
    }

    impl Drop for ComApartment {
        fn drop(&mut self) {
            if self.owned {
                // SAFETY: this balances this thread's successful `CoInitializeEx` call.
                unsafe { CoUninitialize() };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(target_os = "windows"))]
    fn environment() -> AppDirectoryEnvironment {
        AppDirectoryEnvironment {
            home: Some("/home/test".into()),
            ..Default::default()
        }
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn xdg_resolution_should_use_absolute_overrides_and_home_defaults() {
        let environment = AppDirectoryEnvironment {
            xdg_config_home: Some("/explicit/config".into()),
            xdg_data_home: Some("relative/data".into()),
            xdg_state_home: Some(OsString::new()),
            xdg_cache_home: Some("/explicit/cache".into()),
            xdg_runtime_dir: Some("relative/runtime".into()),
            ..environment()
        };

        let directories =
            AppDirectories::resolve_xdg(APP_DIR_NAME, &environment, Some("/temporary".into()))
                .unwrap();

        assert_eq!(directories.config, Path::new("/explicit/config/spaceterm"));
        assert_eq!(
            directories.data,
            Path::new("/home/test/.local/share/spaceterm")
        );
        assert_eq!(
            directories.state,
            Path::new("/home/test/.local/state/spaceterm")
        );
        assert_eq!(directories.cache, Path::new("/explicit/cache/spaceterm"));
        assert_eq!(
            directories.runtime.as_deref(),
            Some(Path::new("/temporary/spaceterm"))
        );
        assert_eq!(
            directories.temporary_directory(),
            Some(Path::new("/temporary"))
        );
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn xdg_resolution_should_not_require_home_when_every_persistent_root_is_explicit() {
        let environment = AppDirectoryEnvironment {
            xdg_config_home: Some("/config".into()),
            xdg_data_home: Some("/data".into()),
            xdg_state_home: Some("/state".into()),
            xdg_cache_home: Some("/cache".into()),
            xdg_runtime_dir: Some("/run/user/1000".into()),
            ..Default::default()
        };

        let directories = AppDirectories::resolve_xdg(APP_DIR_NAME, &environment, None).unwrap();

        assert_eq!(
            directories.runtime.as_deref(),
            Some(Path::new("/run/user/1000/spaceterm"))
        );
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn xdg_resolution_should_leave_runtime_unavailable_without_a_valid_source() {
        let directories = AppDirectories::resolve_xdg(APP_DIR_NAME, &environment(), None).unwrap();

        assert_eq!(directories.runtime, None);
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn temporary_override_should_require_a_nonempty_absolute_normal_path() {
        for value in [
            None,
            Some(OsString::new()),
            Some("relative".into()),
            Some("/tmp/../tmp".into()),
        ] {
            let environment = AppDirectoryEnvironment {
                tmpdir: value,
                ..environment()
            };
            assert_eq!(environment.configured_temporary_root(), None);
        }
        let environment = AppDirectoryEnvironment {
            tmpdir: Some("/private/temporary".into()),
            ..environment()
        };
        assert_eq!(
            environment.configured_temporary_root(),
            Some("/private/temporary".into())
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn host_resolution_should_canonicalize_tmpdir_before_secure_filesystem_use() {
        let environment = AppDirectoryEnvironment {
            tmpdir: Some("/tmp".into()),
            ..environment()
        };
        let fallback_consulted = std::cell::Cell::new(false);

        let directories = AppDirectories::resolve_xdg_for_host(APP_DIR_NAME, &environment, || {
            fallback_consulted.set(true);
            None
        })
        .unwrap();

        let expected = std::fs::canonicalize("/tmp").unwrap();
        let expected_runtime = expected.join(APP_DIR_NAME);
        assert!(!fallback_consulted.get());
        assert_eq!(directories.temporary_directory(), Some(expected.as_path()));
        assert_eq!(
            directories.runtime.as_deref(),
            Some(expected_runtime.as_path())
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn host_resolution_should_use_os_temporary_fallback_or_leave_runtime_unavailable() {
        let environment = AppDirectoryEnvironment {
            tmpdir: Some("relative/temporary".into()),
            ..environment()
        };
        let expected = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let directories = AppDirectories::resolve_xdg_for_host(APP_DIR_NAME, &environment, || {
            Some(std::env::temp_dir())
        })
        .unwrap();
        assert_eq!(directories.temporary_directory(), Some(expected.as_path()));

        let directories =
            AppDirectories::resolve_xdg_for_host(APP_DIR_NAME, &environment, || None).unwrap();
        assert_eq!(directories.temporary_directory(), None);
        assert_eq!(directories.runtime, None);
    }

    #[test]
    fn windows_resolution_should_apply_known_folder_policy_without_xdg_inputs() {
        let root = std::env::current_dir()
            .unwrap()
            .join("spaceterm-windows-policy");
        let roaming = root.join("Roaming");
        let local = root.join("Local");
        let temporary = root.join("Temp");
        let directories = AppDirectories::resolve_windows(
            APP_DIR_NAME,
            roaming.clone(),
            local.clone(),
            Some(temporary.clone()),
        )
        .unwrap();

        assert_eq!(
            directories.config,
            roaming.join("sadiksaifi").join("spaceterm")
        );
        assert_eq!(
            directories.data,
            local.join("sadiksaifi").join("spaceterm").join("Data")
        );
        assert_eq!(
            directories.state,
            local.join("sadiksaifi").join("spaceterm").join("State")
        );
        assert_eq!(
            directories.cache,
            local.join("sadiksaifi").join("spaceterm").join("Cache")
        );
        assert_eq!(
            directories.logs_dir(),
            local.join("sadiksaifi").join("spaceterm").join("Logs")
        );
        assert_eq!(
            directories.managed_ssh_config(),
            local
                .join("sadiksaifi")
                .join("spaceterm")
                .join("Data")
                .join("ssh_config")
        );
        let expected_runtime = temporary
            .join("sadiksaifi")
            .join("spaceterm")
            .join("Runtime");
        assert_eq!(
            directories.runtime.as_deref(),
            Some(expected_runtime.as_path())
        );
        assert_eq!(directories.temporary_directory(), Some(temporary.as_path()));
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn semantic_paths_should_preserve_the_existing_settings_document_name() {
        let directories =
            AppDirectories::resolve_xdg(APP_DIR_NAME, &environment(), Some("/temporary".into()))
                .unwrap();

        assert_eq!(
            directories.config_file(),
            Path::new("/home/test/.config/spaceterm/settings.json")
        );
        assert_eq!(
            directories.managed_ssh_config(),
            Path::new("/home/test/.config/spaceterm/ssh_config")
        );
        assert_eq!(
            directories.logs_dir(),
            Path::new("/home/test/.local/state/spaceterm/logs")
        );
        assert_eq!(
            directories.session_file(),
            Path::new("/home/test/.local/state/spaceterm/session.json")
        );
        assert_eq!(
            directories.window_state_file(),
            Path::new("/home/test/.local/state/spaceterm/window-state.json")
        );
    }

    #[test]
    fn resolution_should_reject_invalid_names_and_native_roots() {
        let native_root = std::env::current_dir()
            .unwrap()
            .join("spaceterm-native-root-policy");
        assert_eq!(
            AppDirectories::resolve_windows(
                "../escape",
                native_root.join("Roaming"),
                native_root.join("Local"),
                None,
            )
            .unwrap_err(),
            DirectoryError::InvalidAppName
        );
        assert_eq!(
            AppDirectories::resolve_windows(
                APP_DIR_NAME,
                "relative/roaming".into(),
                native_root.join("Local"),
                None,
            )
            .unwrap_err(),
            DirectoryError::InvalidNativeRoot {
                root: NativeDirectoryRoot::RoamingAppData
            }
        );
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn debug_output_should_not_expose_resolved_paths() {
        let environment = AppDirectoryEnvironment {
            home: Some("/sensitive/home".into()),
            xdg_runtime_dir: Some("/sensitive/runtime".into()),
            ..Default::default()
        };
        let directories = AppDirectories::resolve_xdg(
            APP_DIR_NAME,
            &environment,
            Some("/sensitive/temporary".into()),
        )
        .unwrap();

        assert_eq!(
            format!("{environment:?}"),
            "AppDirectoryEnvironment(<redacted>)"
        );
        assert_eq!(format!("{directories:?}"), "AppDirectories(..)");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn native_windows_resolution_should_initialize_com_on_a_fresh_thread() {
        std::thread::spawn(|| {
            let directories = AppDirectories::resolve(APP_DIR_NAME).unwrap();

            assert!(directories.config.is_absolute());
            assert!(directories.data.is_absolute());
            assert!(directories.state.is_absolute());
            assert!(directories.cache.is_absolute());
        })
        .join()
        .unwrap();
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn native_windows_resolution_should_preserve_an_existing_mta_apartment() {
        std::thread::spawn(|| {
            use windows_sys::Win32::System::Com::{
                COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize,
            };

            // SAFETY: this fresh thread owns and balances its successful COM initialization.
            let result = unsafe { CoInitializeEx(std::ptr::null(), COINIT_MULTITHREADED as u32) };
            assert!(result >= 0);
            let directories = AppDirectories::resolve(APP_DIR_NAME).unwrap();
            assert!(directories.config.is_absolute());
            // SAFETY: this balances the successful `CoInitializeEx` call above.
            unsafe { CoUninitialize() };
        })
        .join()
        .unwrap();
    }
}
