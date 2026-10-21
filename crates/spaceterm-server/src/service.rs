//! Explicit account service installation and lifecycle. Discovery never starts a service.
pub mod activation;
mod command;
pub mod control;
pub mod maintenance;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
mod registration;
mod runtime;
pub use runtime::CandidateMetadata;

use crate::{ServerError, host::Paths, storage::Directory};
use serde::{Deserialize, Serialize};
use std::{
    ffi::CString,
    fs::File,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[cfg(target_os = "linux")]
use std::process::Stdio;
use std::{
    ffi::OsString,
    io::{Read, Write},
    process::Command,
};

/// The OS authorization surface selected by an explicit local action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LingerAuthorization {
    Terminal,
    Desktop,
    /// SSH prerequisite check that must never request a password or open authorization UI.
    RemoteNoninteractive,
    /// Explicit remote authorization whose password arrives only on inherited standard input.
    RemoteStdin,
}

/// When the owning account's service manager can automatically start this server.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceStartup {
    /// Starts at account login and stops at logout.
    #[default]
    AtLogin,
    /// Starts at boot and survives logout after account startup authorization.
    AtBoot,
}

/// The retained policy and the service manager's current process observation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceStatus {
    pub installed: bool,
    pub enabled: bool,
    pub running: bool,
    pub stopped: bool,
}

/// A signed retained build that can be selected during explicit first installation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetainedInstallation {
    pub directory: PathBuf,
    pub version: String,
    pub commit: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    installation: String,
    stopped: bool,
    installed: bool,
    #[serde(default)]
    removing: bool,
    #[serde(default)]
    run: Option<Run>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    activation: Option<activation::Operation>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Run {
    boot: String,
    token: [u8; 32],
}

/// Local owning-account authority. Construction only resolves account paths.
#[derive(Clone)]
pub struct Service {
    paths: Paths,
    identity: String,
    supervisor: std::sync::Arc<dyn Supervisor>,
}

trait Supervisor: Send + Sync {
    fn startup(&self) -> ServiceStartup;
    fn check(&self, runtime: &Path, removing: bool) -> Result<(), ServerError>;
    fn install(&self, runtime: &Path) -> Result<(), ServerError>;
    fn prepare_start(&self) -> Result<(), ServerError>;
    fn start(&self) -> Result<(), ServerError>;
    fn disable(&self) -> Result<(), ServerError>;
    fn uninstall(&self, runtime: &Path) -> Result<(), ServerError>;
    fn running(&self) -> Result<bool, ServerError>;
    /// Replace only the admitted old definition or finish its exact already-published replacement.
    fn replace(&self, _previous: &Path, _candidate: &Path) -> Result<(), ServerError> {
        Err(ServerError::Unsupported)
    }
    fn observe(&self, runtime: &Path, removing: bool) -> Result<bool, ServerError> {
        self.check(runtime, removing)?;
        self.running()
    }
    fn enable_linger(&self, _authorization: LingerAuthorization) -> Result<(), ServerError> {
        Err(ServerError::Unsupported)
    }
}

impl Service {
    /// The startup policy of the selected service manager; reads no service or credentials.
    pub fn startup(&self) -> ServiceStartup {
        self.supervisor.startup()
    }
    pub fn for_identity(identity: &str) -> Result<Self, ServerError> {
        let paths = Paths::for_identity(identity)?;
        #[cfg(target_os = "macos")]
        let supervisor = std::sync::Arc::new(macos::LaunchAgent::new(&paths, identity)?);
        #[cfg(target_os = "linux")]
        let supervisor = std::sync::Arc::new(linux::UserService::new(&paths, identity)?);
        Ok(Self::new(paths, identity.to_owned(), supervisor))
    }
    fn new(paths: Paths, identity: String, supervisor: std::sync::Arc<dyn Supervisor>) -> Self {
        Self {
            paths,
            identity,
            supervisor,
        }
    }
    /// Explicitly authorize Linux boot startup for this account. This does not start a server.
    pub fn enable_linger(&self, authorization: LingerAuthorization) -> Result<(), ServerError> {
        self.supervisor.enable_linger(authorization)
    }
    pub fn status(&self) -> Result<ServiceStatus, ServerError> {
        self.recover_activation()?;
        let Some((_, state)) = self.read()? else {
            return Ok(ServiceStatus::default());
        };
        self.snapshot(&state)
    }
    /// Discover verified retained builds without selecting, registering, or starting any of them.
    pub fn retained_installations(&self) -> Result<Vec<RetainedInstallation>, ServerError> {
        let retained = self.read()?;
        let _operation = retained
            .as_ref()
            .map(|(data, _)| data.lock("service.lock", false))
            .transpose()?;
        let approved = retained
            .as_ref()
            .map(|(data, _)| Self::state(data))
            .transpose()?
            .flatten();
        let parent = self.paths.data.join("installations");
        match Directory::open(&parent, false) {
            Ok(_) => {}
            Err(ServerError::NotSetUp) => return Ok(Vec::new()),
            Err(error) => return Err(error),
        }
        let directory = owned_directory(&parent)?;
        lock_directory(&directory)?;
        let mut choices = Vec::new();
        for entry in std::fs::read_dir(&parent).map_err(|_| ServerError::StorageUnavailable)? {
            let entry = entry.map_err(|_| ServerError::StorageUnavailable)?;
            let name = entry.file_name();
            if !name.to_str().is_some_and(runtime::digest_valid) {
                continue;
            }
            // Reinstallation restores the approved build; selecting an update is a separate action.
            if approved
                .as_ref()
                .is_some_and(|state| name.to_str() != Some(&state.installation))
            {
                continue;
            }
            let path = parent.join(name);
            let (version, commit) = runtime::describe(&path, &self.identity)?;
            choices.push(RetainedInstallation {
                directory: path,
                version,
                commit,
            });
        }
        verify_directory_path(&directory, &parent)?;
        choices.sort_by(|a, b| (&a.version, &a.commit).cmp(&(&b.version, &b.commit)));
        Ok(choices)
    }
    /// Accept only an independently signed artifact retained in this account's installation store.
    /// Reinstallation preserves the approved version and deliberate Stop state.
    pub fn install(&self, installation: &Path) -> Result<ServiceStatus, ServerError> {
        let data = Directory::open(&self.paths.data, true)?;
        let _operation = data.lock("service.lock", false)?;
        self.install_locked(&data, installation)
    }
    fn install_locked(
        &self,
        data: &Directory,
        installation: &Path,
    ) -> Result<ServiceStatus, ServerError> {
        data.verify_path(&self.paths.data)?;
        self.resume_activation(data)?;
        if let Some(mut state) = Self::state(data)?
            && state.installed
        {
            if state.removing {
                return Err(ServerError::CleanupUnresolved);
            }
            if !state.stopped {
                self.start_locked(data, &mut state, false)?;
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
        let previous = Self::state(data)?;
        let mut state = previous.unwrap_or(State {
            installation: digest.to_owned(),
            stopped: false,
            installed: false,
            removing: false,
            run: None,
            activation: None,
        });
        // Uninstall never authorizes a different runtime. Update activation owns that transition.
        runtime::verify(&self.runtime(&state), &self.identity)?;
        {
            let _setup = data.lock("setup.lock", false)?;
            if Self::state(data)?.is_some() {
                data.read()?;
            } else {
                data.read_or_create()?;
            }
        }
        if !state.stopped {
            self.supervisor.prepare_start()?;
        }
        self.supervisor.install(&self.runtime(&state))?;
        state.installed = true;
        data.write_json(c"service.json", &state)?;
        drop(instance);
        if state.stopped {
            self.supervisor.disable()?;
        } else {
            self.start_locked(data, &mut state, false)?;
        }
        self.snapshot(&state)
    }
    #[cfg(feature = "client")]
    pub(crate) fn local_server(&self) -> Result<crate::LocalServer, ServerError> {
        crate::LocalServer::with_paths(self.paths.clone())
    }
    /// Serialize normal launch with installation and explicit service controls.
    #[cfg(feature = "client")]
    pub(crate) fn launch(
        &self,
        provision: impl FnOnce() -> Result<PathBuf, ServerError>,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<ServiceStatus>, ServerError> {
        if cancelled() {
            return Ok(None);
        }
        let data = Directory::open(&self.paths.data, true)?;
        let deadline = Instant::now() + Duration::from_secs(20);
        let _operation = loop {
            if cancelled() {
                return Ok(None);
            }
            if let Some(lock) = data.try_lock("service.lock")? {
                break lock;
            }
            if Instant::now() >= deadline {
                return Err(ServerError::ServiceUnavailable);
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        data.verify_path(&self.paths.data)?;
        self.resume_activation(&data)?;
        if cancelled() {
            return Ok(None);
        }
        if let Some(mut state) = Self::state(&data)? {
            let status = self.snapshot(&state)?;
            if status.stopped {
                return Ok(Some(status));
            }
            if !status.installed {
                return Err(ServerError::NotInstalled);
            }
            data.read()?;
            if !status.running {
                if cancelled() {
                    return Ok(None);
                }
                self.start_locked(&data, &mut state, false)?;
                return self.snapshot(&state).map(Some);
            }
            return Ok(Some(status));
        }
        // Existing but incompatible identity is never first setup. Leave its exact bytes intact.
        match data.read() {
            Ok(_) | Err(ServerError::NotSetUp) => {}
            Err(error) => return Err(error),
        }
        if cancelled() {
            return Ok(None);
        }
        let installation = provision()?;
        if cancelled() {
            return Ok(None);
        }
        // Registration/start is now admitted. Observe its result even if cancellation arrives.
        self.install_locked(&data, &installation).map(Some)
    }
    /// Explicit Start is the only operation that clears retained Stop intent.
    pub fn start(&self) -> Result<ServiceStatus, ServerError> {
        self.start_existing(true)
    }
    /// Continue requested account work using the approved installation. Never reverses Stop.
    pub fn connect(&self) -> Result<ServiceStatus, ServerError> {
        self.start_existing(false)
    }
    fn start_existing(&self, explicit: bool) -> Result<ServiceStatus, ServerError> {
        let (data, _) = self.read()?.ok_or(ServerError::NotInstalled)?;
        let _operation = data.lock("service.lock", false)?;
        let recovering = Self::state(&data)?.is_some_and(|s| {
            s.activation
                .as_ref()
                .is_some_and(activation::Operation::pending)
        });
        self.resume_activation(&data)?;
        let mut state = Self::state(&data)?.ok_or(ServerError::NotInstalled)?;
        if recovering {
            return self.snapshot(&state);
        }
        if !state.installed {
            return Err(ServerError::NotInstalled);
        }
        if state.stopped && !explicit {
            return Err(ServerError::Stopped);
        }
        self.start_locked(&data, &mut state, explicit)?;
        self.snapshot(&state)
    }
    fn start_locked(
        &self,
        data: &Directory,
        state: &mut State,
        explicit: bool,
    ) -> Result<(), ServerError> {
        let intent = data.lock("service-intent.lock", false)?;
        *state = Self::state(data)?.ok_or(ServerError::StorageUnavailable)?;
        self.supervisor
            .check(&self.runtime(state), state.removing)?;
        if state.stopped && !explicit {
            return Err(ServerError::Stopped);
        }
        if state.removing {
            return Err(ServerError::CleanupUnresolved);
        }
        if self.supervisor.running()? {
            return if state.stopped {
                Err(ServerError::CleanupUnresolved)
            } else {
                Ok(())
            };
        }
        check_cleanup(state)?;
        self.prove_quiescent()?;
        state.run = None;
        data.read()?; // Unavailable or missing identity is never replaced by service controls.
        runtime::verify(&self.runtime(state), &self.identity)?;
        self.supervisor.prepare_start()?;
        state.stopped = false;
        data.write_json(c"service.json", state)?;
        drop(intent);
        let result = self
            .supervisor
            .start()
            .and_then(|()| self.wait_running(true));
        if result.is_err() {
            let _intent = data.lock("service-intent.lock", false)?;
            let mut latest = Self::state(data)?.ok_or(ServerError::StorageUnavailable)?;
            if let Some(operation) = latest.activation.as_mut() {
                operation.startup_failed();
                data.write_json(c"service.json", &latest)?;
            }
        }
        result
    }
    pub fn stop(&self) -> Result<ServiceStatus, ServerError> {
        self.apply_control(control::Action::Stop, None)
    }
    fn stop_locked(&self, data: &Directory, state: &mut State) -> Result<(), ServerError> {
        // Publication precedes all service-manager operations, so partial failure fails closed.
        {
            let _intent = data.lock("service-intent.lock", false)?;
            *state = Self::state(data)?.ok_or(ServerError::StorageUnavailable)?;
            state.stopped = true;
            data.write_json(c"service.json", state)?;
        }
        self.supervisor
            .check(&self.runtime(state), state.removing)?;
        self.supervisor.disable()?;
        self.wait_running(false)?;
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            *state = Self::state(data)?.ok_or(ServerError::StorageUnavailable)?;
            match check_cleanup(state).and_then(|()| self.prove_quiescent()) {
                Ok(()) => return Ok(()),
                Err(ServerError::CleanupUnresolved) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50))
                }
                Err(error) => return Err(error),
            }
        }
    }
    pub fn restart(&self) -> Result<ServiceStatus, ServerError> {
        self.apply_control(control::Action::Restart, None)
    }
    /// Remove only the service registration. Retain identity, organization, artifacts and Stop.
    pub fn uninstall(&self) -> Result<ServiceStatus, ServerError> {
        self.apply_control(control::Action::Uninstall, None)
    }
    fn apply_control(
        &self,
        action: control::Action,
        observed_running: Option<bool>,
    ) -> Result<ServiceStatus, ServerError> {
        let (data, _) = self.read()?.ok_or(ServerError::NotInstalled)?;
        let _operation = data.lock("service.lock", false)?;
        let recovering = Self::state(&data)?.is_some_and(|s| {
            s.activation
                .as_ref()
                .is_some_and(activation::Operation::pending)
        });
        self.resume_activation(&data)?;
        let mut state = Self::state(&data)?.ok_or(ServerError::NotInstalled)?;
        if recovering {
            return self.snapshot(&state);
        }
        if action == control::Action::Uninstall && !state.installed && !state.removing {
            return self.snapshot(&state);
        }
        if !state.installed {
            return Err(ServerError::NotInstalled);
        }
        // Serialize this check with explicit Start and other controls. A quiescent-server
        // confirmation cannot silently become approval to end newly running work.
        if observed_running == Some(false) && self.supervisor.running()? {
            return Err(ServerError::Changed);
        }
        if action == control::Action::Restart && state.stopped {
            return Err(ServerError::Stopped);
        }
        self.stop_locked(&data, &mut state)?;
        match action {
            control::Action::Stop => {}
            control::Action::Restart => self.start_locked(&data, &mut state, true)?,
            control::Action::Uninstall => {
                state.removing = true;
                data.write_json(c"service.json", &state)?;
                self.supervisor.uninstall(&self.runtime(&state))?;
                state.removing = false;
                state.installed = false;
                data.write_json(c"service.json", &state)?;
            }
        }
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
            Err(ServerError::NotSetUp) => return Ok(None),
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
        if let Some(state) = &state
            && let Some(operation) = &state.activation
        {
            operation.validate(state)?;
        }
        Ok(state)
    }
    fn snapshot(&self, state: &State) -> Result<ServiceStatus, ServerError> {
        if state.removing {
            return Err(ServerError::CleanupUnresolved);
        }
        let running = if state.installed {
            self.supervisor
                .observe(&self.runtime(state), state.removing)?
        } else {
            self.supervisor.running()?
        };
        if running && (!state.installed || state.stopped) {
            return Err(ServerError::CleanupUnresolved);
        }
        if !running {
            check_cleanup(state)?;
        }
        Ok(ServiceStatus {
            installed: state.installed,
            enabled: state.installed && !state.stopped,
            stopped: state.stopped,
            running,
        })
    }
    fn wait_running(&self, desired: bool) -> Result<(), ServerError> {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if self.supervisor.running()? == desired
                && (!desired || crate::host::connect(&self.paths).is_ok())
            {
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
            Err(ServerError::NotSetUp) => return Ok(()),
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
}

// Retained Stop also fences explicit foreground startup.
pub(crate) fn check_start(paths: &Paths) -> Result<(), ServerError> {
    let data = match Directory::open(&paths.data, false) {
        Ok(data) => data,
        Err(ServerError::NotSetUp) => return Ok(()),
        Err(error) => return Err(error),
    };
    let state = Service::state(&data)?;
    if state
        .as_ref()
        .is_some_and(|s| s.activation.as_ref().is_some_and(|o| !o.permits_start()))
    {
        return Err(ServerError::ActivationUnresolved);
    }
    if state.is_some_and(|v| v.stopped || !v.installed) {
        return Err(ServerError::Stopped);
    }
    Ok(())
}

/// A refusal observed during Restart is provisional until its service operation completes.
/// Runtime startup uses check_start directly: waiting here from the service itself would
/// deadlock the controller that is waiting for its endpoint to become ready.
pub(crate) fn check_admission(paths: &Paths) -> Result<(), ServerError> {
    match check_start(paths) {
        Err(ServerError::Stopped) => {}
        result => return result,
    }
    let data = Directory::open(&paths.data, false)?;
    let deadline = Instant::now() + Duration::from_secs(20);
    let _operation = loop {
        if let Some(lock) = data.try_lock("service.lock")? {
            break lock;
        }
        if Instant::now() >= deadline {
            return Err(ServerError::ServiceUnavailable);
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    data.verify_path(&paths.data)?;
    check_start(paths)
}

pub(crate) fn check_runtime(paths: &Paths, supervised: bool) -> Result<(), ServerError> {
    check_start(paths)?;
    if !supervised {
        let data = match Directory::open(&paths.data, false) {
            Ok(data) => data,
            Err(ServerError::NotSetUp) => return Ok(()),
            Err(error) => return Err(error),
        };
        if Service::state(&data)?.is_some_and(|v| v.installed) {
            return Err(ServerError::RuntimeUnavailable);
        }
    }
    Ok(())
}

/// Run only the approved artifact. An interrupted same-boot run remains unresolved cleanup.
/// Reboot proves the previous boot's processes ended; ordinary client launches grant no recovery.
pub fn run(
    identity: &str,
    panes: impl crate::pane::PaneService + 'static,
) -> Result<(), ServerError> {
    let service = Service::for_identity(identity)?;
    let (data, _) = service.read()?.ok_or(ServerError::NotInstalled)?;
    let intent = data.lock("service-intent.lock", false)?;
    let mut state = Service::state(&data)?.ok_or(ServerError::NotInstalled)?;
    if state.stopped || !state.installed {
        return Ok(());
    }
    if state
        .activation
        .as_ref()
        .is_some_and(|o| !o.permits_start())
    {
        return Err(ServerError::ActivationUnresolved);
    }
    check_cleanup(&state)?;
    let executable = std::env::current_exe().map_err(|_| ServerError::RuntimeUnavailable)?;
    if executable != service.runtime(&state).join("bin/spaceterm-server") {
        return Err(ServerError::RuntimeUnavailable);
    }
    runtime::verify(&service.runtime(&state), identity)?;
    crate::host::install_shutdown_signals()?;
    let run = Run {
        boot: boot()?,
        token: crate::protocol::random()?,
    };
    activation::admit_run(&mut state, &run)?;
    state.run = Some(run.clone());
    data.write_json(c"service.json", &state)?;
    drop(intent);
    match crate::host::serve_runtime(&service.paths, Some(Box::new(panes)), true) {
        Ok(()) | Err(ServerError::Stopped) => {}
        Err(error) => return Err(error),
    }
    let _intent = data.lock("service-intent.lock", false)?;
    let mut state = Service::state(&data)?.ok_or(ServerError::StorageUnavailable)?;
    if state
        .run
        .as_ref()
        .is_none_or(|current| current.token != run.token)
    {
        return Err(ServerError::CleanupUnresolved);
    }
    state.run = None;
    data.write_json(c"service.json", &state)
}
fn check_cleanup(state: &State) -> Result<(), ServerError> {
    if let Some(run) = &state.run
        && run.boot == boot()?
    {
        return Err(ServerError::CleanupUnresolved);
    }
    Ok(())
}
fn boot() -> Result<String, ServerError> {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: sysctlbyname fills a correctly sized timeval for the fixed kernel key.
        let mut value: libc::timeval = unsafe { std::mem::zeroed() };
        let mut size = std::mem::size_of_val(&value);
        if unsafe {
            libc::sysctlbyname(
                c"kern.boottime".as_ptr(),
                (&raw mut value).cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        } != 0
            || size != std::mem::size_of_val(&value)
        {
            return Err(ServerError::StorageUnavailable);
        }
        Ok(format!("{}:{}", value.tv_sec, value.tv_usec))
    }
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .map(|v| v.trim().to_owned())
            .map_err(|_| ServerError::StorageUnavailable)
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
    let mut prepared = Command::new(program);
    prepared.args(args);
    execute(&mut prepared)
}
fn execute(prepared: &mut Command) -> Result<Vec<u8>, ServerError> {
    command::execute(prepared, Duration::from_secs(20))
}

// An authenticated explicit protocol Shutdown carries Stop intent too. Service-manager signals
// used for logout or crash recovery do not change this policy.
pub(crate) fn retain_stop(paths: &Paths) -> Result<(), ServerError> {
    let data = Directory::open(&paths.data, false)?;
    let _intent = data.lock("service-intent.lock", false)?;
    if let Some(mut state) = Service::state(&data)? {
        if state
            .activation
            .as_ref()
            .is_some_and(activation::Operation::pending)
        {
            return Err(ServerError::ActivationUnresolved);
        }
        state.stopped = true;
        data.write_json(c"service.json", &state)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct TestSupervisor {
        stop_data: Option<PathBuf>,
        running: std::sync::Arc<std::sync::atomic::AtomicBool>,
        allow_stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
        checks: std::sync::atomic::AtomicUsize,
        running_checks: std::sync::atomic::AtomicUsize,
    }
    impl Supervisor for TestSupervisor {
        fn startup(&self) -> ServiceStartup {
            ServiceStartup::AtLogin
        }
        fn check(&self, _: &Path, _: bool) -> Result<(), ServerError> {
            self.checks
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if self.stop_data.is_some() {
                Ok(())
            } else {
                panic!("unexpected OS call")
            }
        }
        fn install(&self, _: &Path) -> Result<(), ServerError> {
            panic!("unexpected OS call")
        }
        fn prepare_start(&self) -> Result<(), ServerError> {
            panic!("unexpected OS call")
        }
        fn start(&self) -> Result<(), ServerError> {
            panic!("unexpected OS call")
        }
        fn disable(&self) -> Result<(), ServerError> {
            let data = Directory::open(
                self.stop_data.as_deref().expect("unexpected OS call"),
                false,
            )?;
            let state = Service::state(&data)?.ok_or(ServerError::NotInstalled)?;
            if !state.stopped {
                return Err(ServerError::StartupFailed);
            }
            if !self.allow_stop.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(ServerError::ServiceUnavailable);
            }
            self.running
                .store(false, std::sync::atomic::Ordering::Relaxed);
            Ok(())
        }
        fn uninstall(&self, _: &Path) -> Result<(), ServerError> {
            panic!("unexpected OS call")
        }
        fn running(&self) -> Result<bool, ServerError> {
            self.running_checks
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if self.stop_data.is_some() {
                Ok(self.running.load(std::sync::atomic::Ordering::Relaxed))
            } else {
                panic!("unexpected OS call")
            }
        }
    }
    pub(super) struct Fixture {
        pub(super) root: PathBuf,
        pub(super) paths: Paths,
    }
    impl Fixture {
        pub(super) fn new() -> Self {
            let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
                "sts-{:x}",
                u64::from_be_bytes(crate::protocol::random().unwrap()[..8].try_into().unwrap())
            ));
            let paths = Paths {
                identity: "spaceterm-development".into(),
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
                    run: None,
                    activation: None,
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
    #[cfg(feature = "client")]
    #[test]
    fn warm_launch_reuses_its_validated_service_observation() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let fixture = Fixture::new();
        let supervisor = Arc::new(TestSupervisor {
            stop_data: Some(fixture.paths.data.clone()),
            running: Arc::new(AtomicBool::new(true)),
            ..Default::default()
        });
        let service = Service::new(
            fixture.paths.clone(),
            fixture.paths.identity.clone(),
            supervisor.clone(),
        );
        let status = service
            .launch(
                || panic!("warm launch must preserve the approved runtime"),
                || false,
            )
            .unwrap()
            .unwrap();
        assert!(status.running && status.installed && !status.stopped);
        assert_eq!(supervisor.checks.load(Ordering::Relaxed), 1);
        assert_eq!(supervisor.running_checks.load(Ordering::Relaxed), 1);
    }
    #[test]
    fn reinstall_discovery_does_not_offer_an_unapproved_staged_build() {
        let fixture = Fixture::new();
        let other = fixture
            .paths
            .data
            .join("installations")
            .join("b".repeat(64));
        Directory::open(other.parent().unwrap(), true).unwrap();
        std::fs::create_dir(&other).unwrap();
        let service = Service::new(
            fixture.paths.clone(),
            "spaceterm-development".into(),
            std::sync::Arc::new(TestSupervisor::default()),
        );
        assert!(service.retained_installations().unwrap().is_empty());
    }
    #[test]
    fn protocol_stop_survives_reopen_without_replacing_identity() {
        let fixture = Fixture::new();
        let seed = Directory::open(&fixture.paths.data, false)
            .unwrap()
            .read()
            .unwrap()
            .server;
        retain_stop(&fixture.paths).unwrap();
        assert_eq!(check_start(&fixture.paths), Err(ServerError::Stopped));
        let reopened = Directory::open(&fixture.paths.data, false).unwrap();
        assert_eq!(reopened.read().unwrap().server, seed);
        assert!(Service::state(&reopened).unwrap().unwrap().stopped);
    }
    #[test]
    fn service_definition_publication_reuses_exact_bytes_and_preserves_changed_entries() {
        let fixture = Fixture::new();
        let registration = registration::Registration {
            directory: fixture.root.join("registration"),
            name: "server.plist".into(),
        };
        registration.install(b"approved definition").unwrap();
        registration.install(b"approved definition").unwrap();
        std::fs::write(registration.path(), b"replacement").unwrap();
        assert_eq!(
            registration.remove(b"approved definition"),
            Err(ServerError::CleanupUnresolved)
        );
        assert_eq!(std::fs::read(registration.path()).unwrap(), b"replacement");
        std::fs::remove_file(registration.path()).unwrap();
        std::os::unix::fs::symlink(fixture.root.join("outside"), registration.path()).unwrap();
        assert!(registration.install(b"approved definition").is_err());
        assert!(
            std::fs::symlink_metadata(registration.path())
                .unwrap()
                .is_symlink()
        );
    }
    #[test]
    fn stop_publishes_durable_intent_before_a_failing_supervisor_operation() {
        let fixture = Fixture::new();
        let supervisor = TestSupervisor {
            stop_data: Some(fixture.paths.data.clone()),
            ..Default::default()
        };
        let service = Service::new(
            fixture.paths.clone(),
            "spaceterm-development".into(),
            std::sync::Arc::new(supervisor),
        );
        assert_eq!(service.stop(), Err(ServerError::ServiceUnavailable));
    }
    #[test]
    fn cancelled_control_preserves_work_and_confirmed_native_refusal_retains_stop() {
        use control::{Action, Controller};
        let fixture = Fixture::new();
        let service = Service::new(
            fixture.paths.clone(),
            "spaceterm-development".into(),
            std::sync::Arc::new(TestSupervisor {
                stop_data: Some(fixture.paths.data.clone()),
                ..Default::default()
            }),
        );
        let control = Controller::new(service);
        drop(control.prepare(Action::Stop).unwrap());
        let data = Directory::open(&fixture.paths.data, false).unwrap();
        assert!(!Service::state(&data).unwrap().unwrap().stopped);
        let confirmation = control.prepare(Action::Stop).unwrap();
        assert_eq!(
            control.confirm(confirmation),
            Err(ServerError::ServiceUnavailable)
        );
        assert!(Service::state(&data).unwrap().unwrap().stopped);
    }
    #[test]
    fn control_confirmation_cannot_target_another_account_service() {
        use control::{Action, Controller};
        let first = Fixture::new();
        let second = Fixture::new();
        let controller = |fixture: &Fixture| {
            Controller::new(Service::new(
                fixture.paths.clone(),
                "spaceterm-development".into(),
                std::sync::Arc::new(TestSupervisor {
                    stop_data: Some(fixture.paths.data.clone()),
                    ..Default::default()
                }),
            ))
        };
        let confirmation = controller(&first).prepare(Action::Stop).unwrap();
        assert_eq!(
            controller(&second).confirm(confirmation),
            Err(ServerError::AuthenticationRefused)
        );
        for fixture in [&first, &second] {
            let data = Directory::open(&fixture.paths.data, false).unwrap();
            assert!(!Service::state(&data).unwrap().unwrap().stopped);
        }
    }
    #[test]
    fn pending_restart_confirmation_cannot_reverse_a_later_deliberate_stop() {
        use control::{Action, Controller};
        let fixture = Fixture::new();
        let control = Controller::new(Service::new(
            fixture.paths.clone(),
            "spaceterm-development".into(),
            std::sync::Arc::new(TestSupervisor {
                stop_data: Some(fixture.paths.data.clone()),
                ..Default::default()
            }),
        ));
        let confirmation = control.prepare(Action::Restart).unwrap();
        retain_stop(&fixture.paths).unwrap();
        assert_eq!(control.confirm(confirmation), Err(ServerError::Stopped));
    }
    #[test]
    fn confirmation_for_a_quiescent_service_cannot_end_newly_running_work() {
        use control::{Action, Controller};
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let fixture = Fixture::new();
        let running = Arc::new(AtomicBool::new(false));
        let control = Controller::new(Service::new(
            fixture.paths.clone(),
            "spaceterm-development".into(),
            Arc::new(TestSupervisor {
                stop_data: Some(fixture.paths.data.clone()),
                running: running.clone(),
                ..Default::default()
            }),
        ));
        let confirmation = control.prepare(Action::Uninstall).unwrap();
        running.store(true, Ordering::Relaxed);
        assert_eq!(control.confirm(confirmation), Err(ServerError::Changed));
        let data = Directory::open(&fixture.paths.data, false).unwrap();
        let state = Service::state(&data).unwrap().unwrap();
        assert!(state.installed && !state.stopped);
    }
    #[test]
    fn a_refused_native_stop_can_be_confirmed_again_without_reversing_stop() {
        use control::{Action, Controller};
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let fixture = Fixture::new();
        let allow_stop = Arc::new(AtomicBool::new(false));
        let control = Controller::new(Service::new(
            fixture.paths.clone(),
            "spaceterm-development".into(),
            Arc::new(TestSupervisor {
                stop_data: Some(fixture.paths.data.clone()),
                running: Arc::new(AtomicBool::new(true)),
                allow_stop: allow_stop.clone(),
                ..Default::default()
            }),
        ));
        let confirmation = control.prepare(Action::Stop).unwrap();
        assert_eq!(
            control.confirm(confirmation),
            Err(ServerError::ServiceUnavailable)
        );
        assert_eq!(control.status(), Err(ServerError::CleanupUnresolved));
        allow_stop.store(true, Ordering::Relaxed);
        let retry = control.prepare(Action::Stop).unwrap();
        let status = control.confirm(retry).unwrap();
        assert!(status.stopped && !status.running && !status.enabled);
    }
    #[test]
    fn foreground_start_cannot_bypass_the_approved_service_runtime() {
        let fixture = Fixture::new();
        assert_eq!(
            crate::host::serve(&fixture.paths),
            Err(ServerError::RuntimeUnavailable)
        );
        assert!(!fixture.paths.socket().exists());
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
    fn restart_cannot_reverse_a_deliberate_stop() {
        let fixture = Fixture::new();
        retain_stop(&fixture.paths).unwrap();
        let service = Service::new(
            fixture.paths.clone(),
            "spaceterm-development".into(),
            std::sync::Arc::new(TestSupervisor::default()),
        );
        assert_eq!(service.restart(), Err(ServerError::Stopped));
        assert_eq!(service.connect(), Err(ServerError::Stopped));
    }
    #[test]
    fn quiescence_refuses_unowned_endpoint_cleanup() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = Fixture::new();
        Directory::open(&fixture.paths.runtime, true).unwrap();
        let socket = fixture.paths.socket();
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
        let service = Service::new(
            fixture.paths.clone(),
            "spaceterm-development".into(),
            std::sync::Arc::new(TestSupervisor::default()),
        );
        assert_eq!(
            service.prove_quiescent(),
            Err(ServerError::CleanupUnresolved)
        );
        drop(listener);
        assert!(socket.exists());
    }

    #[cfg(feature = "client")]
    mod launch {
        use super::*;
        use crate::{LocalStartup, StartupOutcome, pane, protocol};
        use std::sync::{
            Arc, Mutex,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        };

        struct TestPanes;
        impl pane::PaneService for TestPanes {
            fn request(&mut self, _: [u8; 32], _: [u8; 32], request: pane::Request) -> pane::Reply {
                match request {
                    pane::Request::Shutdown => pane::Reply::Closed,
                    pane::Request::Capabilities => pane::Reply::Capabilities {
                        development_pane: true,
                        pane_takeover: false,
                        interactive_input: false,
                        native_effects: false,
                        reading: false,
                        graphics: false,
                        accessibility: false,
                        directory_recovery: false,
                        worktree_writes: false,
                        end_all_sessions: false,
                        workspace_policy: true,
                        presentation_changes: false,
                    },
                    _ => pane::Reply::Refused(pane::Refusal::Unsupported),
                }
            }
            fn disconnected(&mut self, _: [u8; 32]) {}
            fn shutdown(&mut self) -> Result<(), ServerError> {
                Ok(())
            }
        }

        // Substitute the native supervisor only. Storage, verification, startup policy and
        // account-local admission all run their real implementations against private fixtures.
        struct DisableGate {
            entered: std::sync::mpsc::SyncSender<()>,
            proceed: std::sync::mpsc::Receiver<()>,
        }
        struct AccountSupervisor {
            paths: Paths,
            registered: Mutex<Option<PathBuf>>,
            host: Mutex<Option<std::thread::JoinHandle<Result<(), ServerError>>>>,
            authorized: AtomicBool,
            authorizations: AtomicUsize,
            installs: AtomicUsize,
            starts: AtomicUsize,
            observed_running: AtomicBool,
            disable_gate: Mutex<Option<DisableGate>>,
            disable_fails: AtomicBool,
        }
        impl AccountSupervisor {
            fn new(paths: Paths) -> Arc<Self> {
                Arc::new(Self {
                    paths,
                    registered: Mutex::default(),
                    host: Mutex::default(),
                    authorized: AtomicBool::new(true),
                    authorizations: AtomicUsize::new(0),
                    installs: AtomicUsize::new(0),
                    starts: AtomicUsize::new(0),
                    observed_running: AtomicBool::new(false),
                    disable_gate: Mutex::new(None),
                    disable_fails: AtomicBool::new(false),
                })
            }
            fn service(self: &Arc<Self>) -> Service {
                Service::new(
                    self.paths.clone(),
                    self.paths.identity.clone(),
                    self.clone(),
                )
            }
            fn spawn_host(&self, delay: Duration) {
                let paths = self.paths.clone();
                *self.host.lock().unwrap() = Some(std::thread::spawn(move || {
                    std::thread::sleep(delay);
                    crate::host::serve_runtime(&paths, Some(Box::new(TestPanes)), true)
                }));
            }
            fn close_host(&self) -> Result<(), ServerError> {
                let Some(host) = self.host.lock().unwrap().take() else {
                    return Ok(());
                };
                if !host.is_finished() {
                    let deadline = Instant::now() + Duration::from_secs(5);
                    let mut stream = loop {
                        match crate::host::connect(&self.paths) {
                            Ok(stream) => break stream,
                            Err(ServerError::NotRunning) if Instant::now() < deadline => {
                                std::thread::sleep(Duration::from_millis(10));
                            }
                            Err(error) => return Err(error),
                        }
                    };
                    let pin = Directory::open(&self.paths.data, false)?.read()?.server;
                    protocol::admit_client(
                        &mut stream,
                        (&[11; 32], &"Test".into()),
                        &pin,
                        &self.paths.identity,
                        &[protocol::MAJOR],
                    )?;
                    let mut wire = protocol::Wire::terminal(&mut stream)?;
                    wire.write(&pane::Request::Shutdown)?;
                    assert!(matches!(wire.read::<pane::Reply>()?, pane::Reply::Closed));
                }
                host.join().unwrap()
            }
        }
        impl Drop for AccountSupervisor {
            fn drop(&mut self) {
                self.close_host().unwrap();
            }
        }
        impl Supervisor for AccountSupervisor {
            fn startup(&self) -> ServiceStartup {
                ServiceStartup::AtBoot
            }
            fn check(&self, runtime: &Path, _: bool) -> Result<(), ServerError> {
                if self.registered.lock().unwrap().as_deref() == Some(runtime) {
                    Ok(())
                } else {
                    Err(ServerError::CleanupUnresolved)
                }
            }
            fn install(&self, runtime: &Path) -> Result<(), ServerError> {
                self.installs.fetch_add(1, Ordering::Relaxed);
                *self.registered.lock().unwrap() = Some(runtime.to_owned());
                Ok(())
            }
            fn prepare_start(&self) -> Result<(), ServerError> {
                if self.authorized.load(Ordering::Relaxed) {
                    Ok(())
                } else {
                    Err(ServerError::AuthorizationRequired)
                }
            }
            fn start(&self) -> Result<(), ServerError> {
                self.starts.fetch_add(1, Ordering::Relaxed);
                self.spawn_host(Duration::ZERO);
                Ok(())
            }
            fn disable(&self) -> Result<(), ServerError> {
                if let Some(gate) = self.disable_gate.lock().unwrap().take() {
                    gate.entered.send(()).unwrap();
                    gate.proceed.recv_timeout(Duration::from_secs(5)).unwrap();
                }
                if self.disable_fails.load(Ordering::Relaxed) {
                    return Err(ServerError::ServiceUnavailable);
                }
                self.close_host()
            }
            fn uninstall(&self, _: &Path) -> Result<(), ServerError> {
                *self.registered.lock().unwrap() = None;
                Ok(())
            }
            fn running(&self) -> Result<bool, ServerError> {
                self.observed_running.store(true, Ordering::Relaxed);
                Ok(self
                    .host
                    .lock()
                    .unwrap()
                    .as_ref()
                    .is_some_and(|v| !v.is_finished()))
            }
            fn enable_linger(&self, _: LingerAuthorization) -> Result<(), ServerError> {
                self.authorizations.fetch_add(1, Ordering::Relaxed);
                self.authorized.store(true, Ordering::Relaxed);
                Ok(())
            }
        }

        fn no_provision() -> Result<PathBuf, ServerError> {
            panic!("retained policy must never select a different runtime")
        }
        fn ready(outcome: StartupOutcome) -> crate::LocalServer {
            match outcome {
                StartupOutcome::Ready { server, status } => {
                    assert_eq!(status.protocol_major, 1);
                    server
                }
                _ => panic!("launch did not connect"),
            }
        }
        fn fresh(fixture: &Fixture) {
            std::fs::remove_file(fixture.paths.data.join("service.json")).unwrap();
        }
        fn provision(
            service: &Service,
            archive: &Path,
            signature: &Path,
        ) -> Result<PathBuf, ServerError> {
            service
                .provision(
                    archive,
                    signature,
                    runtime::test_support::target(),
                    runtime::test_support::COMMIT,
                    runtime::test_support::VERSION,
                )
                .map(|v| v.directory)
        }

        #[test]
        fn first_launch_and_reopen_reuse_one_service_and_nonsecret_client_identity() {
            let fixture = Fixture::new();
            fresh(&fixture);
            let supervisor = AccountSupervisor::new(fixture.paths.clone());
            let service = supervisor.service();
            let startup = LocalStartup::with_service(service.clone()).unwrap();
            std::fs::remove_file(fixture.paths.data.join("identity.json")).unwrap();
            runtime::test_support::with_artifact(
                &fixture.root.join("artifact"),
                &fixture.paths.identity,
                |archive, signature| {
                    let server = ready(
                        startup
                            .run(|| provision(&service, archive, signature), || false)
                            .unwrap(),
                    );
                    let seed = std::fs::read(fixture.paths.data.join("identity.json")).unwrap();
                    let clients = fixture.paths.data.join("clients/desktop/client.json");
                    let client = std::fs::read(&clients).unwrap();
                    let identity: serde_json::Value = serde_json::from_slice(&client).unwrap();
                    assert_eq!(identity["format"], "account-local-1");
                    assert_eq!(identity.as_object().unwrap().len(), 2);
                    drop(server);
                    drop(startup);
                    // A running service remains reusable even when Linux start authorization is lost.
                    supervisor.authorized.store(false, Ordering::Relaxed);
                    ready(
                        LocalStartup::with_service(service.clone())
                            .unwrap()
                            .run(no_provision, || false)
                            .unwrap(),
                    );
                    assert_eq!(std::fs::read(clients).unwrap(), client);
                    assert_eq!(
                        std::fs::read(fixture.paths.data.join("identity.json")).unwrap(),
                        seed
                    );
                    assert_eq!(supervisor.installs.load(Ordering::Relaxed), 1);
                    assert_eq!(supervisor.starts.load(Ordering::Relaxed), 1);
                },
            );
        }

        #[test]
        fn launch_waits_for_existing_service_ipc_without_restarting_or_reauthorizing() {
            let fixture = Fixture::new();
            let supervisor = AccountSupervisor::new(fixture.paths.clone());
            *supervisor.registered.lock().unwrap() = Some(
                fixture
                    .paths
                    .data
                    .join("installations")
                    .join("a".repeat(64)),
            );
            supervisor.authorized.store(false, Ordering::Relaxed);
            supervisor.spawn_host(Duration::from_millis(100));
            ready(
                LocalStartup::with_service(supervisor.service())
                    .unwrap()
                    .run(no_provision, || false)
                    .unwrap(),
            );
            assert_eq!(supervisor.starts.load(Ordering::Relaxed), 0);
            assert_eq!(supervisor.installs.load(Ordering::Relaxed), 0);
            assert_eq!(supervisor.authorizations.load(Ordering::Relaxed), 0);
        }

        #[test]
        fn refresh_after_launch_and_attachment_wait_for_restart_to_finish() {
            let fixture = Fixture::new();
            fresh(&fixture);
            let supervisor = AccountSupervisor::new(fixture.paths.clone());
            let service = supervisor.service();
            runtime::test_support::with_artifact(
                &fixture.root.join("artifact"),
                &fixture.paths.identity,
                |archive, signature| {
                    let server = ready(
                        LocalStartup::with_service(service.clone())
                            .unwrap()
                            .run(|| provision(&service, archive, signature), || false)
                            .unwrap(),
                    );
                    // This is the boundary after LocalStartup's launch operation releases its lock.
                    assert!(
                        service
                            .launch(no_provision, || false)
                            .unwrap()
                            .unwrap()
                            .running
                    );
                    let (at_disable, disable_entered) = std::sync::mpsc::sync_channel(1);
                    let (resume_disable, proceed_disable) = std::sync::mpsc::sync_channel(1);
                    *supervisor.disable_gate.lock().unwrap() = Some(DisableGate {
                        entered: at_disable,
                        proceed: proceed_disable,
                    });
                    let observe = std::thread::spawn(move || {
                        disable_entered
                            .recv_timeout(Duration::from_secs(5))
                            .unwrap();
                        let refresh_server = server.clone();
                        let (refreshed, refresh_result) = std::sync::mpsc::sync_channel(1);
                        let refresh = std::thread::spawn(move || {
                            refreshed.send(refresh_server.refresh()).unwrap();
                        });
                        let (attached, attach_result) = std::sync::mpsc::sync_channel(1);
                        let attach = std::thread::spawn(move || {
                            attached.send(server.connect_pane()).unwrap();
                        });
                        let early_refresh = refresh_result.recv_timeout(Duration::from_millis(80));
                        let early_attach = attach_result.recv_timeout(Duration::from_millis(80));
                        resume_disable.send(()).unwrap();
                        let refresh_result = early_refresh
                            .or_else(|_| refresh_result.recv_timeout(Duration::from_secs(5)))
                            .unwrap();
                        let attach_result = early_attach
                            .or_else(|_| attach_result.recv_timeout(Duration::from_secs(5)))
                            .unwrap();
                        refresh.join().unwrap();
                        attach.join().unwrap();
                        assert!(
                            refresh_result.is_ok(),
                            "refresh classified Restart as {refresh_result:?}"
                        );
                        assert!(
                            attach_result.is_ok(),
                            "attachment must survive Restart: {:?}",
                            attach_result.err()
                        );
                    });
                    assert!(service.restart().unwrap().running);
                    observe.join().unwrap();
                    assert_eq!(supervisor.starts.load(Ordering::Relaxed), 2);
                },
            );
        }

        #[test]
        fn explicit_stop_queued_during_restart_remains_stopped() {
            let fixture = Fixture::new();
            fresh(&fixture);
            let supervisor = AccountSupervisor::new(fixture.paths.clone());
            let service = supervisor.service();
            runtime::test_support::with_artifact(
                &fixture.root.join("artifact"),
                &fixture.paths.identity,
                |archive, signature| {
                    let server = ready(
                        LocalStartup::with_service(service.clone())
                            .unwrap()
                            .run(|| provision(&service, archive, signature), || false)
                            .unwrap(),
                    );
                    let (at_disable, disable_entered) = std::sync::mpsc::sync_channel(1);
                    let (resume_disable, proceed_disable) = std::sync::mpsc::sync_channel(1);
                    *supervisor.disable_gate.lock().unwrap() = Some(DisableGate {
                        entered: at_disable,
                        proceed: proceed_disable,
                    });
                    let stop_service = service.clone();
                    let observe = std::thread::spawn(move || {
                        disable_entered
                            .recv_timeout(Duration::from_secs(5))
                            .unwrap();
                        let (entered, waiting) = std::sync::mpsc::sync_channel(1);
                        let stop = std::thread::spawn(move || {
                            entered.send(()).unwrap();
                            stop_service.stop()
                        });
                        waiting.recv_timeout(Duration::from_secs(5)).unwrap();
                        std::thread::sleep(Duration::from_millis(80));
                        let pending = !stop.is_finished();
                        resume_disable.send(()).unwrap();
                        assert!(stop.join().unwrap().unwrap().stopped);
                        assert!(pending, "Stop must serialize after the admitted Restart");
                        assert_eq!(server.refresh(), Err(ServerError::Stopped));
                        assert!(matches!(server.connect_pane(), Err(ServerError::Stopped)));
                    });
                    assert!(service.restart().unwrap().running);
                    observe.join().unwrap();
                    assert!(matches!(
                        LocalStartup::with_service(service.clone())
                            .unwrap()
                            .run(no_provision, || false),
                        Ok(StartupOutcome::Stopped)
                    ));
                    assert_eq!(supervisor.starts.load(Ordering::Relaxed), 2);
                },
            );
        }

        #[test]
        fn admission_keeps_deliberate_stop_and_failed_restart_stopped() {
            for failed_restart in [false, true] {
                let fixture = Fixture::new();
                fresh(&fixture);
                let supervisor = AccountSupervisor::new(fixture.paths.clone());
                let service = supervisor.service();
                runtime::test_support::with_artifact(
                    &fixture.root.join("artifact"),
                    &fixture.paths.identity,
                    |archive, signature| {
                        let server = ready(
                            LocalStartup::with_service(service.clone())
                                .unwrap()
                                .run(|| provision(&service, archive, signature), || false)
                                .unwrap(),
                        );
                        let (at_disable, disable_entered) = std::sync::mpsc::sync_channel(1);
                        let (resume_disable, proceed_disable) = std::sync::mpsc::sync_channel(1);
                        *supervisor.disable_gate.lock().unwrap() = Some(DisableGate {
                            entered: at_disable,
                            proceed: proceed_disable,
                        });
                        supervisor
                            .disable_fails
                            .store(failed_restart, Ordering::Relaxed);
                        let observe = std::thread::spawn(move || {
                            disable_entered
                                .recv_timeout(Duration::from_secs(5))
                                .unwrap();
                            let refresh_server = server.clone();
                            let refresh = std::thread::spawn(move || refresh_server.refresh());
                            let attach = std::thread::spawn(move || server.connect_pane());
                            // Both requests overlap the published Stop inside the control operation.
                            std::thread::sleep(Duration::from_millis(80));
                            resume_disable.send(()).unwrap();
                            assert_eq!(refresh.join().unwrap(), Err(ServerError::Stopped));
                            assert!(matches!(attach.join().unwrap(), Err(ServerError::Stopped)));
                        });
                        if failed_restart {
                            assert_eq!(service.restart(), Err(ServerError::ServiceUnavailable));
                        } else {
                            assert!(service.stop().unwrap().stopped);
                        }
                        observe.join().unwrap();
                        let state =
                            Service::state(&Directory::open(&fixture.paths.data, false).unwrap())
                                .unwrap()
                                .unwrap();
                        assert!(state.stopped);
                        assert_eq!(supervisor.starts.load(Ordering::Relaxed), 1);
                    },
                );
            }
        }

        #[test]
        fn uninstalled_stop_bypasses_provision_and_only_explicit_start_can_resume() {
            let fixture = Fixture::new();
            fresh(&fixture);
            let supervisor = AccountSupervisor::new(fixture.paths.clone());
            let service = supervisor.service();
            runtime::test_support::with_artifact(
                &fixture.root.join("artifact"),
                &fixture.paths.identity,
                |archive, signature| {
                    let startup = LocalStartup::with_service(service.clone()).unwrap();
                    ready(
                        startup
                            .run(|| provision(&service, archive, signature), || false)
                            .unwrap(),
                    );
                    service.stop().unwrap();
                    assert!(matches!(
                        startup.run(no_provision, || false),
                        Ok(StartupOutcome::Stopped)
                    ));
                    service.uninstall().unwrap();
                    assert!(matches!(
                        startup.run(no_provision, || false),
                        Ok(StartupOutcome::Stopped)
                    ));
                    // Explicit installation preserves Stop and the approved retained runtime too.
                    let selected = service
                        .retained_installations()
                        .unwrap()
                        .remove(0)
                        .directory;
                    assert!(service.install(&selected).unwrap().stopped);
                    assert_eq!(service.restart(), Err(ServerError::Stopped));
                    assert!(service.start().unwrap().running);
                    ready(startup.run(no_provision, || false).unwrap());
                    assert_eq!(supervisor.starts.load(Ordering::Relaxed), 2);
                },
            );
        }

        #[test]
        fn linux_first_setup_resumes_only_after_explicit_linger_authorization() {
            let fixture = Fixture::new();
            fresh(&fixture);
            let supervisor = AccountSupervisor::new(fixture.paths.clone());
            supervisor.authorized.store(false, Ordering::Relaxed);
            let service = supervisor.service();
            let startup = LocalStartup::with_service(service.clone()).unwrap();
            let seed = std::fs::read(fixture.paths.data.join("identity.json")).unwrap();
            runtime::test_support::with_artifact(
                &fixture.root.join("artifact"),
                &fixture.paths.identity,
                |archive, signature| {
                    for _ in 0..2 {
                        assert!(matches!(
                            startup.run(|| provision(&service, archive, signature), || false),
                            Ok(StartupOutcome::AuthorizationRequired)
                        ));
                    }
                    assert_eq!(supervisor.authorizations.load(Ordering::Relaxed), 0);
                    assert_eq!(supervisor.installs.load(Ordering::Relaxed), 0);
                    assert!(!fixture.paths.data.join("service.json").exists());
                    service.enable_linger(LingerAuthorization::Desktop).unwrap();
                    ready(
                        startup
                            .run(|| provision(&service, archive, signature), || false)
                            .unwrap(),
                    );
                    assert_eq!(supervisor.authorizations.load(Ordering::Relaxed), 1);
                    assert_eq!(supervisor.starts.load(Ordering::Relaxed), 1);
                    assert_eq!(
                        std::fs::read(fixture.paths.data.join("identity.json")).unwrap(),
                        seed
                    );
                },
            );
        }

        #[test]
        fn cancellation_before_admission_leaves_policy_unchanged_and_can_resume() {
            let fixture = Fixture::new();
            fresh(&fixture);
            let supervisor = AccountSupervisor::new(fixture.paths.clone());
            let service = supervisor.service();
            let startup = LocalStartup::with_service(service.clone()).unwrap();
            let cancelled = AtomicBool::new(false);
            runtime::test_support::with_artifact(
                &fixture.root.join("artifact"),
                &fixture.paths.identity,
                |archive, signature| {
                    assert!(matches!(
                        startup.run(
                            || {
                                let selected = provision(&service, archive, signature)?;
                                cancelled.store(true, Ordering::Relaxed);
                                Ok(selected)
                            },
                            || cancelled.load(Ordering::Relaxed)
                        ),
                        Ok(StartupOutcome::Cancelled)
                    ));
                    assert!(!fixture.paths.data.join("service.json").exists());
                    assert_eq!(supervisor.installs.load(Ordering::Relaxed), 0);
                    ready(
                        startup
                            .run(|| provision(&service, archive, signature), || false)
                            .unwrap(),
                    );
                },
            );
        }

        #[test]
        fn cancellation_after_start_admission_reports_the_running_service() {
            let fixture = Fixture::new();
            fresh(&fixture);
            let supervisor = AccountSupervisor::new(fixture.paths.clone());
            let service = supervisor.service();
            runtime::test_support::with_artifact(
                &fixture.root.join("artifact"),
                &fixture.paths.identity,
                |archive, signature| {
                    ready(
                        LocalStartup::with_service(service.clone())
                            .unwrap()
                            .run(
                                || provision(&service, archive, signature),
                                || supervisor.starts.load(Ordering::Relaxed) > 0,
                            )
                            .unwrap(),
                    );
                    assert!(service.status().unwrap().running);
                },
            );
        }

        #[test]
        fn waiting_launch_can_cancel_while_the_service_operation_lock_is_held() {
            let fixture = Fixture::new();
            let data = Directory::open(&fixture.paths.data, false).unwrap();
            let _operation = data.lock("service.lock", false).unwrap();
            let supervisor = AccountSupervisor::new(fixture.paths.clone());
            let checks = AtomicUsize::new(0);
            assert!(matches!(
                LocalStartup::with_service(supervisor.service())
                    .unwrap()
                    .run(no_provision, || checks.fetch_add(1, Ordering::Relaxed) >= 3),
                Ok(StartupOutcome::Cancelled)
            ));
            assert_eq!(supervisor.starts.load(Ordering::Relaxed), 0);
        }

        #[test]
        fn waiting_launch_observes_stop_published_before_it_acquires_the_lock() {
            let fixture = Fixture::new();
            let supervisor = AccountSupervisor::new(fixture.paths.clone());
            *supervisor.registered.lock().unwrap() = Some(
                fixture
                    .paths
                    .data
                    .join("installations")
                    .join("a".repeat(64)),
            );
            let data = Directory::open(&fixture.paths.data, false).unwrap();
            let operation = data.lock("service.lock", false).unwrap();
            let startup = LocalStartup::with_service(supervisor.service()).unwrap();
            let (entered, waiting) = std::sync::mpsc::channel();
            let worker = std::thread::spawn(move || {
                let sent = AtomicBool::new(false);
                startup.run(no_provision, || {
                    if !sent.swap(true, Ordering::Relaxed) {
                        entered.send(()).unwrap();
                    }
                    false
                })
            });
            waiting.recv_timeout(Duration::from_secs(5)).unwrap();
            retain_stop(&fixture.paths).unwrap();
            drop(operation);
            assert!(matches!(
                worker.join().unwrap(),
                Ok(StartupOutcome::Stopped)
            ));
            assert_eq!(supervisor.starts.load(Ordering::Relaxed), 0);
        }

        #[test]
        fn launch_lock_storage_failure_is_never_retried_as_contention() {
            let fixture = Fixture::new();
            std::os::unix::fs::symlink(
                fixture.root.join("outside"),
                fixture.paths.data.join("service.lock"),
            )
            .unwrap();
            let supervisor = AccountSupervisor::new(fixture.paths.clone());
            let checks = AtomicUsize::new(0);
            assert!(matches!(
                LocalStartup::with_service(supervisor.service())
                    .unwrap()
                    .run(no_provision, || checks.fetch_add(1, Ordering::Relaxed) >= 2,),
                Err(ServerError::UnsafeStorage)
            ));
            assert_eq!(checks.load(Ordering::Relaxed), 2);
            assert!(fixture.paths.data.join("service.lock").is_symlink());
        }

        #[test]
        fn concurrent_launches_provision_and_start_one_approved_runtime() {
            let fixture = Fixture::new();
            fresh(&fixture);
            let supervisor = AccountSupervisor::new(fixture.paths.clone());
            let service = supervisor.service();
            let barrier = std::sync::Barrier::new(2);
            let provisions = AtomicUsize::new(0);
            std::thread::scope(|scope| {
                for index in 0..2 {
                    let source = fixture.root.join(format!("artifact-{index}"));
                    let service = &service;
                    let barrier = &barrier;
                    let provisions = &provisions;
                    scope.spawn(move || {
                        runtime::test_support::with_artifact(
                            &source,
                            &service.identity,
                            |archive, signature| {
                                barrier.wait();
                                ready(
                                    LocalStartup::with_service(service.clone())
                                        .unwrap()
                                        .run(
                                            || {
                                                provisions.fetch_add(1, Ordering::Relaxed);
                                                provision(service, archive, signature)
                                            },
                                            || false,
                                        )
                                        .unwrap(),
                                );
                            },
                        );
                    });
                }
            });
            assert_eq!(provisions.load(Ordering::Relaxed), 1);
            assert_eq!(supervisor.installs.load(Ordering::Relaxed), 1);
            assert_eq!(supervisor.starts.load(Ordering::Relaxed), 1);
        }

        #[test]
        fn dormant_service_respects_cancellation_then_starts_its_approved_runtime() {
            let fixture = Fixture::new();
            fresh(&fixture);
            let supervisor = AccountSupervisor::new(fixture.paths.clone());
            let service = supervisor.service();
            runtime::test_support::with_artifact(
                &fixture.root.join("artifact"),
                &fixture.paths.identity,
                |archive, signature| {
                    let selected = provision(&service, archive, signature).unwrap();
                    let data = Directory::open(&fixture.paths.data, false).unwrap();
                    data.write_json(
                        c"service.json",
                        &State {
                            installation: selected
                                .file_name()
                                .unwrap()
                                .to_str()
                                .unwrap()
                                .to_owned(),
                            installed: true,
                            stopped: false,
                            removing: false,
                            run: None,
                            activation: None,
                        },
                    )
                    .unwrap();
                    *supervisor.registered.lock().unwrap() = Some(selected.clone());
                    let policy = std::fs::read(fixture.paths.data.join("service.json")).unwrap();
                    let startup = LocalStartup::with_service(service.clone()).unwrap();
                    let outcome = startup.run(no_provision, || {
                        supervisor.observed_running.load(Ordering::Relaxed)
                    });
                    assert_eq!(supervisor.starts.load(Ordering::Relaxed), 0);
                    assert!(matches!(outcome, Ok(StartupOutcome::Cancelled)));
                    assert_eq!(
                        std::fs::read(fixture.paths.data.join("service.json")).unwrap(),
                        policy
                    );
                    ready(
                        LocalStartup::with_service(service.clone())
                            .unwrap()
                            .run(no_provision, || false)
                            .unwrap(),
                    );
                    assert_eq!(*supervisor.registered.lock().unwrap(), Some(selected));
                    assert_eq!(supervisor.installs.load(Ordering::Relaxed), 0);
                    assert_eq!(supervisor.starts.load(Ordering::Relaxed), 1);
                },
            );
        }

        #[test]
        fn dormant_start_refuses_an_unowned_live_endpoint_before_starting_a_process() {
            use std::os::unix::{fs::PermissionsExt, net::UnixListener};
            let fixture = Fixture::new();
            let supervisor = AccountSupervisor::new(fixture.paths.clone());
            *supervisor.registered.lock().unwrap() = Some(
                fixture
                    .paths
                    .data
                    .join("installations")
                    .join("a".repeat(64)),
            );
            Directory::open(&fixture.paths.runtime, true).unwrap();
            let _listener = UnixListener::bind(fixture.paths.socket()).unwrap();
            std::fs::set_permissions(
                fixture.paths.socket(),
                std::fs::Permissions::from_mode(0o600),
            )
            .unwrap();
            assert!(matches!(
                LocalStartup::with_service(supervisor.service())
                    .unwrap()
                    .run(no_provision, || false),
                Err(ServerError::CleanupUnresolved)
            ));
            assert!(fixture.paths.socket().exists());
            assert_eq!(supervisor.starts.load(Ordering::Relaxed), 0);
        }

        #[test]
        fn an_incompatible_running_peer_is_refused_without_retry_or_runtime_selection() {
            use std::os::unix::{fs::PermissionsExt, net::UnixListener};
            let fixture = Fixture::new();
            let supervisor = AccountSupervisor::new(fixture.paths.clone());
            *supervisor.registered.lock().unwrap() = Some(
                fixture
                    .paths
                    .data
                    .join("installations")
                    .join("a".repeat(64)),
            );
            Directory::open(&fixture.paths.runtime, true).unwrap();
            let listener = UnixListener::bind(fixture.paths.socket()).unwrap();
            std::fs::set_permissions(
                fixture.paths.socket(),
                std::fs::Permissions::from_mode(0o600),
            )
            .unwrap();
            *supervisor.host.lock().unwrap() = Some(std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut wire = protocol::Wire::terminal(&mut stream)?;
                let hello: serde_json::Value = wire.read()?;
                wire.write(&serde_json::json!({"selected":0, "build":hello["build"], "server":hello["server"]}))
            }));
            assert!(matches!(
                LocalStartup::with_service(supervisor.service())
                    .unwrap()
                    .run(no_provision, || false),
                Err(ServerError::IncompatibleProtocol)
            ));
            assert_eq!(supervisor.starts.load(Ordering::Relaxed), 0);
            assert_eq!(supervisor.installs.load(Ordering::Relaxed), 0);
            supervisor
                .host
                .lock()
                .unwrap()
                .take()
                .unwrap()
                .join()
                .unwrap()
                .unwrap();
        }

        #[test]
        fn incompatible_identity_and_unresolved_cleanup_never_invoke_provision() {
            let fixture = Fixture::new();
            fresh(&fixture);
            let supervisor = AccountSupervisor::new(fixture.paths.clone());
            let startup = LocalStartup::with_service(supervisor.service()).unwrap();
            let data = Directory::open(&fixture.paths.data, false).unwrap();
            data.write_json(
                c"identity.json",
                &serde_json::json!({"format":"old", "server":[1]}),
            )
            .unwrap();
            let identity = std::fs::read(fixture.paths.data.join("identity.json")).unwrap();
            assert!(matches!(
                startup.run(no_provision, || false),
                Err(ServerError::IncompatibleState)
            ));
            assert_eq!(
                std::fs::read(fixture.paths.data.join("identity.json")).unwrap(),
                identity
            );
            data.write_json(
                c"service.json",
                &State {
                    installation: "a".repeat(64),
                    installed: true,
                    stopped: false,
                    removing: true,
                    run: None,
                    activation: None,
                },
            )
            .unwrap();
            assert!(matches!(
                startup.run(no_provision, || false),
                Err(ServerError::CleanupUnresolved)
            ));
            assert_eq!(supervisor.starts.load(Ordering::Relaxed), 0);
        }

        #[test]
        fn first_setup_rejects_a_sibling_executable_without_publishing_policy() {
            let fixture = Fixture::new();
            fresh(&fixture);
            let supervisor = AccountSupervisor::new(fixture.paths.clone());
            let sibling = fixture.root.join("spaceterm-server");
            std::fs::write(&sibling, b"unverified sibling").unwrap();
            assert!(matches!(
                LocalStartup::with_service(supervisor.service())
                    .unwrap()
                    .run(|| Ok(sibling.clone()), || false),
                Err(ServerError::RuntimeUnavailable)
            ));
            assert!(!fixture.paths.data.join("service.json").exists());
            assert_eq!(supervisor.installs.load(Ordering::Relaxed), 0);
        }
    }
}
