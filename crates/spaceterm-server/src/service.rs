//! Explicit account service installation and lifecycle. Discovery never starts a service.
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
mod registration;
mod runtime;

use crate::{ServerError, host::Paths, storage::Directory};
use serde::{Deserialize, Serialize};
use std::{
    ffi::{CString, OsString},
    fs::File,
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

/// The retained policy and the service manager's current process observation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ServiceStatus {
    pub installed: bool,
    pub enabled: bool,
    pub running: bool,
    pub stopped: bool,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    installation: String,
    stopped: bool,
    installed: bool,
    #[serde(default)]
    removing: bool,
}

/// Local owning-account authority. Construction only resolves account paths.
#[derive(Clone)]
pub struct Service {
    paths: Paths,
    identity: String,
}

impl Service {
    pub fn for_identity(identity: &str) -> Result<Self, ServerError> {
        #[cfg(not(target_os = "macos"))]
        {
            let _ = identity;
            Err(ServerError::Unsupported)
        }
        #[cfg(target_os = "macos")]
        Ok(Self {
            paths: Paths::for_identity(identity)?,
            identity: identity.to_owned(),
        })
    }
    pub fn status(&self) -> Result<ServiceStatus, ServerError> {
        let Some((_, state)) = self.read()? else {
            return Ok(ServiceStatus::default());
        };
        self.snapshot(&state)
    }
    /// Accept only an independently signed artifact retained in this account's installation store.
    /// Reinstallation preserves the approved version and deliberate Stop state.
    pub fn install(&self, installation: &Path) -> Result<ServiceStatus, ServerError> {
        let data = Directory::open(&self.paths.data, true)?;
        let _operation = data.lock("service.lock", false)?;
        data.verify_path(&self.paths.data)?;
        if let Some(state) = Self::state(&data)?
            && state.installed
        {
            if state.removing {
                return Err(ServerError::CleanupUnresolved);
            }
            return self.snapshot(&state);
        }
        let runtime_directory = Directory::open(&self.paths.runtime, true)?;
        let instance = runtime_directory
            .lock("server.lock", true)
            .map_err(|_| ServerError::CleanupUnresolved)?;
        let digest = installation
            .file_name()
            .and_then(|v| v.to_str())
            .filter(|v| runtime::digest_valid(v))
            .ok_or(ServerError::RuntimeUnavailable)?;
        if installation != self.paths.data.join("installations").join(digest) {
            return Err(ServerError::RuntimeUnavailable);
        }
        let installations = owned_directory(&self.paths.data.join("installations"))?;
        // This directory lock is shared with the signed-package retention task.
        lock_directory(&installations)?;
        runtime::verify(installation, &self.identity)?;
        let previous = Self::state(&data)?;
        let mut state = previous.unwrap_or(State {
            installation: digest.to_owned(),
            stopped: false,
            installed: false,
            removing: false,
        });
        // Uninstall never authorizes a different runtime. Update activation owns that transition.
        runtime::verify(&self.runtime(&state), &self.identity)?;
        {
            let _setup = data.lock("setup.lock", false)?;
            if Self::state(&data)?.is_some() {
                data.read()?;
            } else {
                data.read_or_create()?;
            }
        }
        if !state.stopped {
            self.platform()?.prepare_start()?;
        }
        self.platform()?.install(&self.runtime(&state))?;
        state.installed = true;
        data.write_json(c"service.json", &state)?;
        drop(instance);
        if state.stopped {
            self.platform()?.disable()?;
        } else {
            self.start_locked(&data, &mut state)?;
        }
        self.snapshot(&state)
    }
    /// Explicit Start is the only operation that clears retained Stop intent.
    pub fn start(&self) -> Result<ServiceStatus, ServerError> {
        let (data, _) = self.read()?.ok_or(ServerError::NotInstalled)?;
        let _operation = data.lock("service.lock", false)?;
        let mut state = Self::state(&data)?.ok_or(ServerError::NotInstalled)?;
        if !state.installed {
            return Err(ServerError::NotInstalled);
        }
        self.start_locked(&data, &mut state)?;
        self.snapshot(&state)
    }
    #[cfg(all(feature = "client", target_os = "macos"))]
    pub(crate) fn start_if_enabled(&self) -> Result<(), ServerError> {
        let (data, _) = self.read()?.ok_or(ServerError::NotInstalled)?;
        let _operation = data.lock("service.lock", false)?;
        let mut state = Self::state(&data)?.ok_or(ServerError::NotInstalled)?;
        if state.stopped || !state.installed {
            return Err(ServerError::Stopped);
        }
        self.start_locked(&data, &mut state)
    }
    fn start_locked(&self, data: &Directory, state: &mut State) -> Result<(), ServerError> {
        if state.removing {
            return Err(ServerError::CleanupUnresolved);
        }
        data.read()?; // Unavailable or missing identity is never replaced by service controls.
        runtime::verify(&self.runtime(state), &self.identity)?;
        self.platform()?.prepare_start()?;
        state.stopped = false;
        data.write_json(c"service.json", state)?;
        self.platform()?.start()?;
        self.wait_running(true)
    }
    pub fn stop(&self) -> Result<ServiceStatus, ServerError> {
        let (data, _) = self.read()?.ok_or(ServerError::NotInstalled)?;
        let _operation = data.lock("service.lock", false)?;
        let mut state = Self::state(&data)?.ok_or(ServerError::NotInstalled)?;
        self.stop_locked(&data, &mut state)?;
        self.snapshot(&state)
    }
    fn stop_locked(&self, data: &Directory, state: &mut State) -> Result<(), ServerError> {
        // Publication precedes all service-manager operations, so partial failure fails closed.
        state.stopped = true;
        data.write_json(c"service.json", state)?;
        self.platform()?.disable()?;
        self.wait_running(false)?;
        self.prove_quiescent()
    }
    pub fn restart(&self) -> Result<ServiceStatus, ServerError> {
        let (data, _) = self.read()?.ok_or(ServerError::NotInstalled)?;
        let _operation = data.lock("service.lock", false)?;
        let mut state = Self::state(&data)?.ok_or(ServerError::NotInstalled)?;
        if !state.installed {
            return Err(ServerError::NotInstalled);
        }
        self.stop_locked(&data, &mut state)?;
        self.start_locked(&data, &mut state)?;
        self.snapshot(&state)
    }
    /// Remove only the service registration. Retain identity, organization, artifacts and Stop.
    pub fn uninstall(&self) -> Result<ServiceStatus, ServerError> {
        let (data, _) = self.read()?.ok_or(ServerError::NotInstalled)?;
        let _operation = data.lock("service.lock", false)?;
        let mut state = Self::state(&data)?.ok_or(ServerError::NotInstalled)?;
        self.stop_locked(&data, &mut state)?;
        state.removing = true;
        data.write_json(c"service.json", &state)?;
        self.platform()?.uninstall(&self.runtime(&state))?;
        state.removing = false;
        state.installed = false;
        data.write_json(c"service.json", &state)?;
        self.snapshot(&state)
    }
    fn runtime(&self, state: &State) -> PathBuf {
        self.paths
            .data
            .join("installations")
            .join(&state.installation)
    }
    fn read(&self) -> Result<Option<(Directory, State)>, ServerError> {
        let data = match Directory::open(&self.paths.data, false) {
            Ok(data) => data,
            Err(ServerError::NotPaired) => return Ok(None),
            Err(error) => return Err(error),
        };
        Ok(Self::state(&data)?.map(|state| (data, state)))
    }
    fn state(data: &Directory) -> Result<Option<State>, ServerError> {
        let state: Option<State> = data.read_json("service.json")?;
        if state
            .as_ref()
            .is_some_and(|v| !runtime::digest_valid(&v.installation))
        {
            return Err(ServerError::UnsafeStorage);
        }
        Ok(state)
    }
    fn snapshot(&self, state: &State) -> Result<ServiceStatus, ServerError> {
        Ok(ServiceStatus {
            installed: state.installed,
            enabled: state.installed && !state.stopped,
            stopped: state.stopped,
            running: self.platform()?.running()?,
        })
    }
    fn wait_running(&self, desired: bool) -> Result<(), ServerError> {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if self.platform()?.running()? == desired {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(if desired {
                    ServerError::StartupFailed
                } else {
                    ServerError::CleanupUnresolved
                });
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    fn prove_quiescent(&self) -> Result<(), ServerError> {
        let directory = match Directory::open(&self.paths.runtime, false) {
            Ok(directory) => directory,
            Err(ServerError::NotPaired) => return Ok(()),
            Err(error) => return Err(error),
        };
        let _instance = directory
            .lock("server.lock", true)
            .map_err(|_| ServerError::CleanupUnresolved)?;
        directory.verify_path(&self.paths.runtime)?;
        if let Some(identity) = directory.socket_identity(c"s")? {
            if !matches!(
                crate::host::connect_socket(&self.paths.socket()),
                Err(crate::host::SocketUnavailable::Refused)
            ) {
                return Err(ServerError::CleanupUnresolved);
            }
            directory.verify_path(&self.paths.runtime)?;
            directory.remove_socket(c"s", identity)?;
        }
        Ok(())
    }
    #[cfg(target_os = "macos")]
    fn platform(&self) -> Result<macos::LaunchAgent, ServerError> {
        macos::LaunchAgent::new(&self.paths, &self.identity)
    }
    #[cfg(not(target_os = "macos"))]
    fn platform(&self) -> Result<Unavailable, ServerError> {
        Err(ServerError::ServiceUnavailable)
    }
}

// Retained Stop also fences explicit foreground and temporary desktop startup paths.
pub(crate) fn check_start(paths: &Paths) -> Result<(), ServerError> {
    let data = match Directory::open(&paths.data, false) {
        Ok(data) => data,
        Err(ServerError::NotPaired) => return Ok(()),
        Err(error) => return Err(error),
    };
    if Service::state(&data)?.is_some_and(|v| v.stopped || !v.installed) {
        return Err(ServerError::Stopped);
    }
    Ok(())
}

/// Service entry point admission. The selected executable must match the approved artifact.
pub fn admit(identity: &str) -> Result<bool, ServerError> {
    let service = Service::for_identity(identity)?;
    let (_, state) = service.read()?.ok_or(ServerError::NotInstalled)?;
    if state.stopped || !state.installed {
        return Ok(false);
    }
    let executable = std::env::current_exe().map_err(|_| ServerError::RuntimeUnavailable)?;
    if executable != service.runtime(&state).join("bin/spaceterm-server") {
        return Err(ServerError::RuntimeUnavailable);
    }
    runtime::verify(&service.runtime(&state), identity)?;
    Ok(true)
}

#[cfg(not(target_os = "macos"))]
struct Unavailable;
#[cfg(not(target_os = "macos"))]
impl Unavailable {
    fn install(&self, _: &Path) -> Result<(), ServerError> {
        Err(ServerError::ServiceUnavailable)
    }
    fn prepare_start(&self) -> Result<(), ServerError> {
        Err(ServerError::ServiceUnavailable)
    }
    fn start(&self) -> Result<(), ServerError> {
        Err(ServerError::ServiceUnavailable)
    }
    fn disable(&self) -> Result<(), ServerError> {
        Err(ServerError::ServiceUnavailable)
    }
    fn uninstall(&self, _: &Path) -> Result<(), ServerError> {
        Err(ServerError::ServiceUnavailable)
    }
    fn running(&self) -> Result<bool, ServerError> {
        Err(ServerError::ServiceUnavailable)
    }
}

fn owned_directory(path: &Path) -> Result<File, ServerError> {
    if !path.is_absolute() {
        return Err(ServerError::UnsafeStorage);
    }
    open_relative(
        &File::open("/").map_err(|_| ServerError::StorageUnavailable)?,
        path.strip_prefix("/")
            .map_err(|_| ServerError::UnsafeStorage)?,
        true,
    )
}
fn open_relative(directory: &File, path: &Path, is_directory: bool) -> Result<File, ServerError> {
    let mut current = directory
        .try_clone()
        .map_err(|_| ServerError::StorageUnavailable)?;
    let components: Vec<_> = path.components().collect();
    for (index, part) in components.iter().enumerate() {
        let std::path::Component::Normal(name) = part else {
            return Err(ServerError::UnsafeStorage);
        };
        let name = CString::new(name.as_bytes()).map_err(|_| ServerError::UnsafeStorage)?;
        let last = index + 1 == components.len();
        let flags = libc::O_RDONLY
            | libc::O_NOFOLLOW
            | libc::O_CLOEXEC
            | libc::O_NONBLOCK
            | if !last || is_directory {
                libc::O_DIRECTORY
            } else {
                0
            };
        // SAFETY: the directory and terminated component are retained through openat.
        let fd = unsafe { libc::openat(current.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(ServerError::StorageUnavailable);
        }
        // SAFETY: openat transferred a fresh owned descriptor.
        current = unsafe { File::from_raw_fd(fd) };
        let metadata = current
            .metadata()
            .map_err(|_| ServerError::StorageUnavailable)?;
        // System ancestors may be root-owned. Every entry must resist changes by other accounts.
        // SAFETY: geteuid has no preconditions.
        if (metadata.uid() != 0 && metadata.uid() != unsafe { libc::geteuid() })
            || metadata.mode() & 0o022 != 0
        {
            // /tmp is a sticky system ancestor; descendants still require account ownership.
            if !(metadata.uid() == 0 && metadata.is_dir() && metadata.mode() & 0o1000 != 0 && !last)
            {
                return Err(ServerError::UnsafeStorage);
            }
        }
        if last && metadata.uid() != unsafe { libc::geteuid() } {
            return Err(ServerError::UnsafeStorage);
        }
    }
    Ok(current)
}
fn verify_directory_path(directory: &File, path: &Path) -> Result<(), ServerError> {
    let now = owned_directory(path)?;
    let a = directory
        .metadata()
        .map_err(|_| ServerError::StorageUnavailable)?;
    let b = now
        .metadata()
        .map_err(|_| ServerError::StorageUnavailable)?;
    if a.dev() != b.dev() || a.ino() != b.ino() {
        return Err(ServerError::UnsafeStorage);
    }
    Ok(())
}
fn lock_directory(directory: &File) -> Result<(), ServerError> {
    // SAFETY: flock operates on a retained directory descriptor.
    if unsafe { libc::flock(directory.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(ServerError::StorageUnavailable);
    }
    Ok(())
}
fn command(program: &str, args: &[OsString]) -> Result<Vec<u8>, ServerError> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| ServerError::ServiceUnavailable)?;
    let mut output = child.stdout.take().ok_or(ServerError::ServiceUnavailable)?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = output.by_ref().take(65537).read_to_end(&mut bytes);
        bytes
    });
    let deadline = Instant::now() + Duration::from_secs(20);
    let result = loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                break if status.success() {
                    Ok(())
                } else {
                    Err(ServerError::ServiceUnavailable)
                };
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(ServerError::ServiceUnavailable);
            }
        }
    };
    let bytes = reader.join().map_err(|_| ServerError::ServiceUnavailable)?;
    result?;
    if bytes.len() > 65536 {
        return Err(ServerError::ServiceUnavailable);
    }
    Ok(bytes)
}

// An authenticated explicit protocol Shutdown carries Stop intent too. Service-manager signals
// used for logout or crash recovery do not change this policy.
pub(crate) fn retain_stop(paths: &Paths) -> Result<(), ServerError> {
    let data = Directory::open(&paths.data, false)?;
    if let Some(mut state) = Service::state(&data)? {
        state.stopped = true;
        data.write_json(c"service.json", &state)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture {
        root: PathBuf,
        paths: Paths,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
                "sts-{:x}",
                u64::from_be_bytes(crate::protocol::random().unwrap()[..8].try_into().unwrap())
            ));
            let paths = Paths {
                data: root.join("data"),
                runtime: root.join("runtime"),
            };
            let data = Directory::open(&paths.data, true).unwrap();
            data.read_or_create().unwrap();
            data.write_json(
                c"service.json",
                &State {
                    installation: "a".repeat(64),
                    stopped: false,
                    installed: true,
                    removing: false,
                },
            )
            .unwrap();
            Self { root, paths }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
    #[test]
    fn protocol_stop_survives_reopen_without_replacing_identity() {
        let fixture = Fixture::new();
        let seed = Directory::open(&fixture.paths.data, false)
            .unwrap()
            .read()
            .unwrap()
            .seed;
        retain_stop(&fixture.paths).unwrap();
        assert_eq!(check_start(&fixture.paths), Err(ServerError::Stopped));
        let reopened = Directory::open(&fixture.paths.data, false).unwrap();
        assert_eq!(reopened.read().unwrap().seed, seed);
        assert!(Service::state(&reopened).unwrap().unwrap().stopped);
    }
    #[test]
    fn unavailable_policy_is_never_treated_as_missing_installation() {
        let fixture = Fixture::new();
        let data = Directory::open(&fixture.paths.data, false).unwrap();
        data.write_json(
            c"service.json",
            &serde_json::json!({"installation":"broken"}),
        )
        .unwrap();
        assert_eq!(check_start(&fixture.paths), Err(ServerError::UnsafeStorage));
    }
    #[test]
    fn quiescence_refuses_unowned_endpoint_cleanup() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = Fixture::new();
        Directory::open(&fixture.paths.runtime, true).unwrap();
        let socket = fixture.paths.socket();
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
        let service = Service {
            paths: fixture.paths.clone(),
            identity: "spaceterm-development".into(),
        };
        assert_eq!(
            service.prove_quiescent(),
            Err(ServerError::CleanupUnresolved)
        );
        drop(listener);
        assert!(socket.exists());
    }
}
