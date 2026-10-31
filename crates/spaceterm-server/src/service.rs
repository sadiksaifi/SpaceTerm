//! Explicit account service installation and lifecycle. Discovery never starts a service.
pub mod activation;
mod command;
pub mod control;
pub mod feed;
pub mod helper;
pub mod lane;
pub mod lane_codes;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
pub mod operations;
mod registration;
mod runtime;
pub mod tokens;
#[cfg(feature = "client")]
pub use runtime::CliUninstall;
pub use runtime::{
    CandidateMetadata, CliLauncher, CliLink, Layout, bundle_identifier, bundle_name,
    verify_candidate_bytes,
};

use crate::{
    ServerError,
    host::Paths,
    storage::{Directory, LockWait},
};
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
    /// The automatic-start preference, retained independently of deliberate Stop.
    pub enabled: bool,
    pub running: bool,
    pub stopped: bool,
}

/// The exact stopped or dormant installation authorized during temporary SSH setup.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetupStartObservation {
    pub account: u32,
    pub build: String,
    pub server: [u8; 32],
    pub installation: String,
    pub version: String,
    pub commit: String,
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
    automatic_start: bool,
    installed: bool,
    removing: bool,
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
    fn stop(&self) -> Result<(), ServerError>;
    /// How long Stop waits for the server to exit. Restart and Update stop through the same wait.
    /// A server still running afterwards leaves the operation unresolved.
    fn stop_timeout(&self) -> Duration;
    /// Change startup activation only. Never stop, start or unregister the service.
    fn set_automatic_start(&self, enabled: bool) -> Result<(), ServerError>;
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
    /// Whether boot startup is authorized, where the service manager has that notion.
    fn linger_authorized(&self) -> Result<Option<bool>, ServerError> {
        Ok(None)
    }
}

impl Service {
    /// The startup policy of the selected service manager; reads no service or credentials.
    pub fn startup(&self) -> ServiceStartup {
        self.supervisor.startup()
    }
    pub fn for_identity(identity: &str) -> Result<Self, ServerError> {
        Self::for_paths(Paths::for_identity(identity)?)
    }
    /// The service of the account whose server state lives at `paths`.
    pub(crate) fn for_paths(paths: Paths) -> Result<Self, ServerError> {
        let identity = paths.identity.clone();
        let identity = identity.as_str();
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
    /// Where this account's signed installations are retained.
    pub fn installations_directory(&self) -> PathBuf {
        self.paths.data.join("installations")
    }
    /// The owning account's home, from its account record rather than the environment.
    pub fn account_home(&self) -> Option<PathBuf> {
        self.paths.data.ancestors().nth(4).map(Path::to_owned)
    }
    /// Whether Linux boot startup is authorized for this account. `None` where startup follows
    /// login instead. This never requests authorization.
    pub fn linger(&self) -> Result<Option<bool>, ServerError> {
        self.supervisor.linger_authorized()
    }
    /// Explicitly authorize Linux boot startup for this account. This does not start a server.
    pub fn enable_linger(&self, authorization: LingerAuthorization) -> Result<(), ServerError> {
        self.supervisor.enable_linger(authorization)
    }
    /// Change only automatic startup using owning-account host authority.
    /// A deliberate Stop remains in force until explicit Start, regardless of this preference.
    /// Linux requires confirmed linger before automatic startup can be enabled.
    pub fn set_automatic_start(&self, enabled: bool) -> Result<ServiceStatus, ServerError> {
        let (data, _) = self.read()?.ok_or(ServerError::NotInstalled)?;
        let _operation = data.lock("service.lock", false)?;
        let _intent = data.lock("service-intent.lock", false)?;
        let mut state = Self::state(&data)?.ok_or(ServerError::NotInstalled)?;
        if state
            .activation
            .as_ref()
            .is_some_and(|operation| !operation.settled())
        {
            return Err(ServerError::ActivationUnresolved);
        }
        if !state.installed {
            return Err(ServerError::NotInstalled);
        }
        self.snapshot(&state)?;
        if enabled {
            self.supervisor.prepare_start()?;
        }
        // Retain the requested policy before changing native activation. A failed call may be
        // retried with the same value without dispatching any service lifecycle action.
        state.automatic_start = enabled;
        data.write_json(c"service.json", &state)?;
        self.supervisor
            .set_automatic_start(enabled && !state.stopped)?;
        self.snapshot(&state)
    }
    pub fn status(&self) -> Result<ServiceStatus, ServerError> {
        self.recover_activation()?;
        let Some((_, state)) = self.read()? else {
            return Ok(ServiceStatus::default());
        };
        self.snapshot(&state)
    }
    /// Observe service policy and the supervisor without continuing an admitted activation.
    /// This never provisions, starts, stops, stages or recovers a server.
    /// An unresolved or failed activation returns ActivationUnresolved for review by its owner.
    pub fn passive_status(&self) -> Result<ServiceStatus, ServerError> {
        let Some((_, state)) = self.read()? else {
            return Ok(ServiceStatus::default());
        };
        if state
            .activation
            .as_ref()
            .is_some_and(|operation| !operation.settled())
        {
            return Err(ServerError::ActivationUnresolved);
        }
        self.snapshot(&state)
    }
    /// Read only an existing account server identity. Absence never creates server state.
    pub fn passive_server_id(&self) -> Result<Option<[u8; 32]>, ServerError> {
        match Directory::open(&self.paths.data, false).and_then(|data| data.read()) {
            Ok(record) => Ok(Some(record.server)),
            Err(ServerError::NotSetUp) => Ok(None),
            Err(error) => Err(error),
        }
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
        let _directory = lock_directory(&directory)?;
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
        let _installations = lock_directory(&installations)?;
        runtime::verify(installation, &self.identity)?;
        let previous = Self::state(data)?;
        let mut state = previous.unwrap_or(State {
            installation: digest.to_owned(),
            stopped: false,
            automatic_start: true,
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
        // A launcher failure never blocks the service; clients read it as Outdated.
        self.repoint_cli_locked(&self.runtime(&state));
        drop(instance);
        if state.stopped {
            self.supervisor.stop()?;
        } else {
            self.start_locked(data, &mut state, false)?;
        }
        self.snapshot(&state)
    }
    #[cfg(feature = "client")]
    pub(crate) fn local_server(&self) -> Result<crate::ServerConnection, ServerError> {
        crate::ServerConnection::with_paths(self.paths.clone())
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
        if cancelled() {
            return Ok(None);
        }
        // Reads and a concurrent launch's start usually finish within the grace. A longer
        // operation of any kind, such as Update, Restart, Stop or Start, is presented by the
        // client while it waits through wait_for_operation.
        let _operation = match data.wait_lock("service.lock", OPERATION_GRACE, Some(&cancelled))? {
            LockWait::Locked(lock) => lock,
            LockWait::Held => return Err(ServerError::ServiceBusy),
            LockWait::Cancelled => return Ok(None),
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
    /// Wait for the service operation that launch reported as ServiceBusy, at most
    /// OPERATION_LIMIT. Run launch again afterwards: the operation may have stopped the server.
    #[cfg(feature = "client")]
    pub(crate) fn wait_for_operation(
        &self,
        cancelled: impl Fn() -> bool,
    ) -> Result<(), ServerError> {
        let data = Directory::open(&self.paths.data, true)?;
        match data.wait_lock("service.lock", OPERATION_LIMIT, Some(&cancelled))? {
            LockWait::Locked(_) | LockWait::Cancelled => Ok(()),
            LockWait::Held => Err(ServerError::ServiceBusy),
        }
    }
    /// Explicit Start is the only operation that clears retained Stop intent.
    pub fn start(&self) -> Result<ServiceStatus, ServerError> {
        self.start_existing(true)
    }
    /// Continue requested account work using the approved installation. Never reverses Stop.
    pub fn connect(&self) -> Result<ServiceStatus, ServerError> {
        self.start_existing(false)
    }
    /// Observe the approved installation without recovering or starting a service.
    pub fn setup_start_observation(&self) -> Result<Option<SetupStartObservation>, ServerError> {
        let Some((data, _)) = self.read()? else {
            return Ok(None);
        };
        let _operation = data.lock("service.lock", false)?;
        let _intent = data.lock("service-intent.lock", false)?;
        let state = Self::state(&data)?.ok_or(ServerError::NotInstalled)?;
        if !state.installed {
            return Ok(None);
        }
        self.check_setup_start_state(&state)?;
        let (version, commit) = runtime::describe(&self.runtime(&state), &self.identity)?;
        Ok(Some(SetupStartObservation {
            // SAFETY: geteuid has no preconditions.
            account: unsafe { libc::geteuid() },
            build: self.identity.clone(),
            server: data.read()?.server,
            installation: state.installation,
            version,
            commit,
            stopped: state.stopped,
        }))
    }
    /// Start only the installation and server named by this setup approval.
    /// Validation and Stop publication share the service and intent locks.
    pub fn start_for_setup(
        &self,
        approved: &SetupStartObservation,
    ) -> Result<ServiceStatus, ServerError> {
        let (data, _) = self.read()?.ok_or(ServerError::NotInstalled)?;
        let _operation = data.lock("service.lock", false)?;
        let mut state = Self::state(&data)?.ok_or(ServerError::NotInstalled)?;
        self.start_observed_locked(&data, &mut state, true, Some(approved))?;
        self.snapshot(&state)
    }
    fn check_setup_start_state(&self, state: &State) -> Result<(), ServerError> {
        if !state.installed || state.removing {
            return Err(ServerError::Changed);
        }
        if state
            .activation
            .as_ref()
            .is_some_and(|operation| !operation.settled())
        {
            return Err(ServerError::ActivationUnresolved);
        }
        Ok(())
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
        self.start_observed_locked(data, state, explicit, None)
    }
    fn start_observed_locked(
        &self,
        data: &Directory,
        state: &mut State,
        explicit: bool,
        approved: Option<&SetupStartObservation>,
    ) -> Result<(), ServerError> {
        let intent = data.lock("service-intent.lock", false)?;
        *state = Self::state(data)?.ok_or(ServerError::StorageUnavailable)?;
        if let Some(approved) = approved {
            self.check_setup_start_state(state)?;
            // SAFETY: geteuid has no preconditions.
            if approved.account != unsafe { libc::geteuid() }
                || approved.build != self.identity
                || approved.server != data.read()?.server
                || approved.installation != state.installation
                || approved.stopped != state.stopped
            {
                return Err(ServerError::Changed);
            }
            let (version, commit) = runtime::describe(&self.runtime(state), &self.identity)?;
            if approved.version != version || approved.commit != commit {
                return Err(ServerError::Changed);
            }
        }
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
            .and_then(|()| self.supervisor.set_automatic_start(state.automatic_start))
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
    /// Confirm an already completed Stop without issuing another native service action.
    pub fn stop_if_stopped(&self) -> Result<Option<ServiceStatus>, ServerError> {
        let Some((data, _)) = self.read()? else {
            return Ok(None);
        };
        let _operation = data.lock("service.lock", false)?;
        let state = Self::state(&data)?.ok_or(ServerError::NotInstalled)?;
        if !state.stopped || !state.installed {
            return Ok(None);
        }
        if state
            .activation
            .as_ref()
            .is_some_and(|operation| !operation.settled())
        {
            return Err(ServerError::ActivationUnresolved);
        }
        let status = self.snapshot(&state)?;
        self.prove_quiescent()?;
        Ok(Some(status))
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
        self.supervisor.stop()?;
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
        self.apply_control_locked(&data, action, observed_running)
    }
    fn apply_control_locked(
        &self,
        data: &Directory,
        action: control::Action,
        observed_running: Option<bool>,
    ) -> Result<ServiceStatus, ServerError> {
        let recovering = Self::state(data)?.is_some_and(|s| {
            s.activation
                .as_ref()
                .is_some_and(activation::Operation::pending)
        });
        self.resume_activation(data)?;
        let mut state = Self::state(data)?.ok_or(ServerError::NotInstalled)?;
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
        if action == control::Action::Stop && state.stopped {
            match self.snapshot(&state) {
                Ok(status) => {
                    self.prove_quiescent()?;
                    return Ok(status);
                }
                // An explicit Stop may retry incomplete native cleanup. This is not a no-op.
                Err(ServerError::CleanupUnresolved) => {}
                Err(error) => return Err(error),
            }
        }
        self.stop_locked(data, &mut state)?;
        match action {
            control::Action::Stop => {}
            control::Action::Restart => self.start_locked(data, &mut state, true)?,
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
            enabled: state.installed && state.automatic_start,
            stopped: state.stopped,
            running,
        })
    }
    fn wait_running(&self, desired: bool) -> Result<(), ServerError> {
        let deadline = Instant::now()
            + if desired {
                Duration::from_secs(15)
            } else {
                self.supervisor.stop_timeout()
            };
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

/// How long launch waits for another service operation before it reports ServiceBusy, and
/// how long a caller that must answer quickly waits at admission. Reads usually finish
/// within it.
pub(crate) const OPERATION_GRACE: Duration = Duration::from_secs(1);

/// How long a launch or admission waits for a service operation that holds service.lock.
/// The longest legitimate hold is a Restart or Update: Linux's 90 s stop, then up to 15 s for
/// cleanup and 15 s for startup. Only a stuck operation outlasts this limit, and the wait then
/// reports ServiceBusy instead of waiting forever.
#[cfg(feature = "client")]
pub(crate) const OPERATION_LIMIT: Duration = Duration::from_secs(180);

/// A refusal observed during Restart or Update is provisional until its service operation
/// completes, so admission waits for it, at most `limit`. `cancelled` ends the wait early with
/// ServiceBusy; without it the wait blocks until the operation ends or the limit passes.
/// Runtime startup uses check_start directly: waiting here from the service itself would
/// deadlock the controller that is waiting for its endpoint to become ready.
pub(crate) fn check_admission(
    paths: &Paths,
    limit: Duration,
    cancelled: Option<&dyn Fn() -> bool>,
) -> Result<(), ServerError> {
    match check_start(paths) {
        Err(ServerError::Stopped | ServerError::ActivationUnresolved) => {}
        result => return result,
    }
    let data = Directory::open(&paths.data, false)?;
    let _operation = match data.wait_lock("service.lock", limit, cancelled)? {
        LockWait::Locked(lock) => lock,
        LockWait::Held | LockWait::Cancelled => return Err(ServerError::ServiceBusy),
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
pub fn run(identity: &str, panes: crate::PaneFactory) -> Result<(), ServerError> {
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
    let layout = runtime::Layout::host(identity).ok_or(ServerError::RuntimeUnavailable)?;
    if executable != service.runtime(&state).join(layout.executable()) {
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
    match crate::host::serve_runtime(&service.paths, Some(panes), true) {
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
fn lock_directory(directory: &File) -> Result<crate::storage::Lock, ServerError> {
    crate::storage::Lock::directory(directory)
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
        automatic_start: std::sync::atomic::AtomicBool,
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
            assert!(self.stop_data.is_some(), "unexpected OS call");
            Ok(())
        }
        fn set_automatic_start(&self, enabled: bool) -> Result<(), ServerError> {
            self.automatic_start
                .store(enabled, std::sync::atomic::Ordering::Relaxed);
            Ok(())
        }
        fn start(&self) -> Result<(), ServerError> {
            panic!("unexpected OS call")
        }
        fn stop(&self) -> Result<(), ServerError> {
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
            self.set_automatic_start(false)?;
            self.running
                .store(false, std::sync::atomic::Ordering::Relaxed);
            Ok(())
        }
        fn stop_timeout(&self) -> Duration {
            Duration::from_secs(15)
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
                data: root.join(".local/share/spaceterm-development/server"),
                runtime: root.join("runtime"),
            };
            let data = Directory::open(&paths.data, true).unwrap();
            data.read_or_create().unwrap();
            data.write_json(
                c"service.json",
                &State {
                    installation: "a".repeat(64),
                    stopped: false,
                    automatic_start: true,
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
    #[test]
    fn an_already_quiescent_stop_is_a_no_op_and_incomplete_cleanup_is_not_success() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let fixture = Fixture::new();
        let running = Arc::new(AtomicBool::new(false));
        let service = Service::new(
            fixture.paths.clone(),
            fixture.paths.identity.clone(),
            Arc::new(TestSupervisor {
                stop_data: Some(fixture.paths.data.clone()),
                running: running.clone(),
                ..Default::default()
            }),
        );
        let data = Directory::open(&fixture.paths.data, false).unwrap();
        let mut state = Service::state(&data).unwrap().unwrap();
        state.stopped = true;
        data.write_json(c"service.json", &state).unwrap();
        let retained = std::fs::read(fixture.paths.data.join("service.json")).unwrap();
        assert!(service.stop_if_stopped().unwrap().is_some());
        assert!(service.stop().unwrap().stopped);
        assert_eq!(
            std::fs::read(fixture.paths.data.join("service.json")).unwrap(),
            retained
        );
        running.store(true, Ordering::Relaxed);
        assert_eq!(
            service.stop_if_stopped(),
            Err(ServerError::CleanupUnresolved)
        );
        assert_eq!(service.stop(), Err(ServerError::ServiceUnavailable));
    }
    #[test]
    fn setup_start_refuses_replaced_installation_server_or_stop_before_native_effects() {
        use runtime::test_support::{COMMIT, VERSION, target, with_artifact};
        let fixture = Fixture::new();
        let service = Service::new(
            fixture.paths.clone(),
            fixture.paths.identity.clone(),
            std::sync::Arc::new(TestSupervisor::default()),
        );
        with_artifact(
            &fixture.root.join("signed"),
            &fixture.paths.identity,
            |archive, signature| {
                let installed = service
                    .provision(archive, signature, target(), COMMIT, VERSION)
                    .unwrap();
                let data = Directory::open(&fixture.paths.data, false).unwrap();
                let mut original = Service::state(&data).unwrap().unwrap();
                original.installation = installed
                    .directory
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_owned();
                original.stopped = true;
                data.write_json(c"service.json", &original).unwrap();
                let identity = data.read().unwrap();
                let approved = service.setup_start_observation().unwrap().unwrap();
                for changed in 0..4 {
                    let mut state = original.clone();
                    let mut current_identity = data.read().unwrap();
                    current_identity.server = identity.server;
                    let mut request = approved.clone();
                    match changed {
                        0 => state.installation = "b".repeat(64),
                        1 => current_identity.server[0] ^= 1,
                        2 => state.stopped = false,
                        _ => request.commit = "b".repeat(40),
                    }
                    data.write_json(c"service.json", &state).unwrap();
                    data.write(&current_identity).unwrap();
                    let service_bytes =
                        std::fs::read(fixture.paths.data.join("service.json")).unwrap();
                    let identity_bytes =
                        std::fs::read(fixture.paths.data.join("identity.json")).unwrap();
                    assert_eq!(service.start_for_setup(&request), Err(ServerError::Changed));
                    assert_eq!(
                        std::fs::read(fixture.paths.data.join("service.json")).unwrap(),
                        service_bytes
                    );
                    assert_eq!(
                        std::fs::read(fixture.paths.data.join("identity.json")).unwrap(),
                        identity_bytes
                    );
                }
            },
        );
    }
    #[test]
    fn automatic_start_changes_preserve_the_running_server_and_its_retained_state() {
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
        let data = Directory::open(&fixture.paths.data, false).unwrap();
        let mut original = Service::state(&data).unwrap().unwrap();
        original.run = Some(Run {
            boot: "retained-boot".into(),
            token: [7; 32],
        });
        data.write_json(c"service.json", &original).unwrap();
        let identity = std::fs::read(fixture.paths.data.join("identity.json")).unwrap();
        let controller = control::Controller::new(service.clone());
        for enabled in [false, true, false] {
            let status = controller.set_automatic_start(enabled).unwrap();
            assert_eq!(
                status,
                ServiceStatus {
                    installed: true,
                    enabled,
                    running: true,
                    stopped: false
                }
            );
            let mut expected = original.clone();
            expected.automatic_start = enabled;
            assert_eq!(Service::state(&data).unwrap().unwrap(), expected);
            assert_eq!(
                std::fs::read(fixture.paths.data.join("identity.json")).unwrap(),
                identity
            );
            assert_eq!(supervisor.automatic_start.load(Ordering::Relaxed), enabled);
        }
        assert!(service.passive_status().unwrap().running);
    }

    #[test]
    fn automatic_start_changes_never_reverse_stop_and_stop_keeps_the_preference() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let fixture = Fixture::new();
        let supervisor = Arc::new(TestSupervisor {
            stop_data: Some(fixture.paths.data.clone()),
            allow_stop: Arc::new(AtomicBool::new(true)),
            ..Default::default()
        });
        let service = Service::new(
            fixture.paths.clone(),
            fixture.paths.identity.clone(),
            supervisor.clone(),
        );
        let stopped = service.stop().unwrap();
        assert!(stopped.enabled && stopped.stopped && !stopped.running);
        for enabled in [false, true] {
            let status = service.set_automatic_start(enabled).unwrap();
            assert_eq!(
                status,
                ServiceStatus {
                    installed: true,
                    enabled,
                    running: false,
                    stopped: true
                }
            );
            assert!(!supervisor.automatic_start.load(Ordering::Relaxed));
            assert_eq!(service.connect(), Err(ServerError::Stopped));
            assert_eq!(check_start(&fixture.paths), Err(ServerError::Stopped));
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
        assert!(status.stopped && !status.running && status.enabled);
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
        impl crate::connection::service::PaneService for TestPanes {
            fn admitted(
                &mut self,
                _: &crate::connection::service::Peer,
                _: pane::control::ClientName,
                _: crate::connection::outbox::Outbox,
            ) {
            }
            fn request(
                &mut self,
                _: &crate::connection::service::Peer,
                _: protocol::RequestId,
                operation: protocol::Operation,
            ) -> crate::connection::service::Dispatch {
                crate::connection::service::Dispatch::reply(match operation {
                    protocol::Operation::Shutdown => Ok(protocol::ReplyBody::Done),
                    _ => Err(pane::Refusal::Unsupported),
                })
            }
            fn input(&mut self, _: &crate::connection::service::Peer, _: protocol::Input) {}
            fn view(&mut self, _: &crate::connection::service::Peer, _: protocol::View) {}
            fn effect_done(
                &mut self,
                _: &crate::connection::service::Peer,
                _: protocol::EffectDone,
            ) {
            }
            fn acknowledge(&mut self, _: &crate::connection::service::Peer, _: u64) {}
            fn disconnected(&mut self, _: &crate::connection::service::Peer) {}
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
        /// A child that another thread forked and that has not reached exec yet. It holds a copy
        /// of every descriptor open at the fork until it is released.
        struct UnexecutedChild {
            release: std::os::unix::net::UnixStream,
            process: libc::pid_t,
        }
        impl UnexecutedChild {
            fn fork() -> Self {
                use std::os::fd::AsRawFd;
                let (release, gate) = std::os::unix::net::UnixStream::pair().unwrap();
                // SAFETY: the child uses only read and _exit before terminating, with no Rust cleanup.
                let process = unsafe { libc::fork() };
                assert!(process >= 0);
                if process == 0 {
                    let mut byte = 0_u8;
                    // SAFETY: the inherited descriptor and one-byte writable buffer are live.
                    unsafe {
                        libc::read(gate.as_raw_fd(), (&raw mut byte).cast(), 1);
                        libc::_exit(0);
                    }
                }
                Self { release, process }
            }
        }
        impl Drop for UnexecutedChild {
            fn drop(&mut self) {
                use std::io::Write;
                let _ = self.release.write_all(&[1]);
                // SAFETY: this reaps only the child created above; no other process is signalled.
                unsafe {
                    libc::waitpid(self.process, std::ptr::null_mut(), 0);
                }
            }
        }
        struct AccountSupervisor {
            paths: Paths,
            registered: Mutex<Option<PathBuf>>,
            host: Mutex<Option<std::thread::JoinHandle<Result<(), ServerError>>>>,
            authorized: AtomicBool,
            authorizations: AtomicUsize,
            installs: AtomicUsize,
            starts: AtomicUsize,
            automatic_start: AtomicBool,
            observed_running: AtomicBool,
            disable_gate: Mutex<Option<DisableGate>>,
            disable_fails: AtomicBool,
            fork_children: AtomicBool,
            children: Mutex<Vec<UnexecutedChild>>,
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
                    automatic_start: AtomicBool::new(false),
                    observed_running: AtomicBool::new(false),
                    disable_gate: Mutex::new(None),
                    disable_fails: AtomicBool::new(false),
                    fork_children: AtomicBool::new(false),
                    children: Mutex::default(),
                })
            }
            fn service(self: &Arc<Self>) -> Service {
                Service::new(
                    self.paths.clone(),
                    self.paths.identity.clone(),
                    self.clone(),
                )
            }
            /// Stand in for another thread spawning a process during this native operation.
            fn fork_child(&self) {
                if self.fork_children.load(Ordering::Relaxed) {
                    self.children.lock().unwrap().push(UnexecutedChild::fork());
                }
            }
            fn spawn_host(&self, delay: Duration) {
                let paths = self.paths.clone();
                *self.host.lock().unwrap() = Some(std::thread::spawn(move || {
                    std::thread::sleep(delay);
                    crate::host::serve_runtime(&paths, Some(crate::panes(|_| Ok(TestPanes))), true)
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
                    let link = crate::host::link(stream, Arc::new(|_| {}))?;
                    assert!(matches!(
                        link.request(protocol::Operation::Shutdown)?.wait(),
                        Ok(protocol::ReplyBody::Done)
                    ));
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
                self.fork_child();
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
            fn set_automatic_start(&self, enabled: bool) -> Result<(), ServerError> {
                self.automatic_start.store(enabled, Ordering::Relaxed);
                Ok(())
            }
            fn stop(&self) -> Result<(), ServerError> {
                self.set_automatic_start(false)?;
                if let Some(gate) = self.disable_gate.lock().unwrap().take() {
                    gate.entered.send(()).unwrap();
                    gate.proceed.recv_timeout(Duration::from_secs(5)).unwrap();
                }
                if self.disable_fails.load(Ordering::Relaxed) {
                    return Err(ServerError::ServiceUnavailable);
                }
                self.fork_child();
                self.close_host()
            }
            fn stop_timeout(&self) -> Duration {
                Duration::from_secs(15)
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
        fn ready(outcome: StartupOutcome) -> crate::ServerConnection {
            match outcome {
                StartupOutcome::Ready { server, status } => {
                    assert_eq!(status.protocol_major, protocol::MAJOR);
                    server
                }
                _ => panic!("launch did not connect"),
            }
        }
        /// Launch as the desktop does: wait out a service operation that launch reports, then
        /// launch again.
        fn launch_settled(
            startup: &LocalStartup,
            mut provision: impl FnMut() -> Result<PathBuf, ServerError>,
        ) -> StartupOutcome {
            loop {
                match startup.run(&mut provision, || false).unwrap() {
                    StartupOutcome::ServiceBusy => startup.wait_for_service(|| false).unwrap(),
                    outcome => return outcome,
                }
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
        fn automatic_start_and_explicit_start_preserve_registration_and_require_linger() {
            let fixture = Fixture::new();
            fresh(&fixture);
            let supervisor = AccountSupervisor::new(fixture.paths.clone());
            let service = supervisor.service();
            runtime::test_support::with_artifact(
                &fixture.root.join("artifact"),
                &fixture.paths.identity,
                |archive, signature| {
                    let installation = provision(&service, archive, signature).unwrap();
                    service.install(&installation).unwrap();
                    service.set_automatic_start(false).unwrap();
                    assert_eq!(
                        supervisor.registered.lock().unwrap().as_ref(),
                        Some(&installation)
                    );
                    let stopped = service.stop().unwrap();
                    assert!(stopped.stopped && !stopped.enabled && !stopped.running);
                    supervisor.authorized.store(false, Ordering::Relaxed);
                    assert_eq!(
                        service.set_automatic_start(true),
                        Err(ServerError::AuthorizationRequired)
                    );
                    assert_eq!(service.passive_status().unwrap(), stopped);
                    supervisor.authorized.store(true, Ordering::Relaxed);
                    let enabled = service.set_automatic_start(true).unwrap();
                    assert!(enabled.enabled && enabled.stopped && !enabled.running);
                    assert!(!supervisor.automatic_start.load(Ordering::Relaxed));
                    service.set_automatic_start(false).unwrap();
                    let started = service.start().unwrap();
                    assert!(started.running && !started.stopped && !started.enabled);
                    assert!(!supervisor.automatic_start.load(Ordering::Relaxed));
                    assert_eq!(
                        supervisor.registered.lock().unwrap().as_ref(),
                        Some(&installation)
                    );
                    assert_eq!(supervisor.installs.load(Ordering::Relaxed), 1);
                    assert_eq!(supervisor.starts.load(Ordering::Relaxed), 2);
                },
            );
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
                            attached.send(server.connect(Arc::new(|_| {}))).unwrap();
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
                        assert!(matches!(
                            server.connect(Arc::new(|_| {})),
                            Err(ServerError::Stopped)
                        ));
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
                            let attach =
                                std::thread::spawn(move || server.connect(Arc::new(|_| {})));
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
        fn locks_end_with_their_owner_while_an_unexecuted_child_holds_inherited_copies() {
            let fixture = Fixture::new();
            fresh(&fixture);
            let supervisor = AccountSupervisor::new(fixture.paths.clone());
            // Registration runs under the launch's instance lock, and Stop runs under the
            // server's instance lock. Each forked child keeps a copy of those descriptors.
            supervisor.fork_children.store(true, Ordering::Relaxed);
            let service = supervisor.service();
            runtime::test_support::with_artifact(
                &fixture.root.join("artifact"),
                &fixture.paths.identity,
                |archive, signature| {
                    ready(
                        LocalStartup::with_service(service.clone())
                            .unwrap()
                            .run(|| provision(&service, archive, signature), || false)
                            .unwrap(),
                    );
                    assert!(service.stop().unwrap().stopped);
                    assert!(service.start().unwrap().running);
                    assert_eq!(supervisor.children.lock().unwrap().len(), 2);
                },
            );
            supervisor.children.lock().unwrap().clear();
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
                let observed = || {
                    if !sent.swap(true, Ordering::Relaxed) {
                        entered.send(()).unwrap();
                    }
                    false
                };
                // A launch that outlasts its grace waits for the operation and runs again, as
                // the desktop does, so the answer never depends on when the lock is released.
                loop {
                    match startup.run(no_provision, observed) {
                        Ok(StartupOutcome::ServiceBusy) => {
                            startup.wait_for_service(|| false).unwrap()
                        }
                        outcome => break outcome,
                    }
                }
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

        /// A service operation that outlasts the launch grace, as a slow Linux stop during
        /// Restart or Update does, is reported, then waited for. Launch opens once it ends.
        #[test]
        fn launch_reports_a_long_service_operation_then_opens_once_it_ends() {
            let fixture = Fixture::new();
            let supervisor = AccountSupervisor::new(fixture.paths.clone());
            *supervisor.registered.lock().unwrap() = Some(
                fixture
                    .paths
                    .data
                    .join("installations")
                    .join("a".repeat(64)),
            );
            supervisor.spawn_host(Duration::ZERO);
            let data = Directory::open(&fixture.paths.data, false).unwrap();
            let operation = data.lock("service.lock", false).unwrap();
            let startup = LocalStartup::with_service(supervisor.service()).unwrap();
            assert!(matches!(
                startup.run(no_provision, || false),
                Ok(StartupOutcome::ServiceBusy)
            ));
            let held = Duration::from_millis(500);
            let operation = std::thread::spawn(move || {
                std::thread::sleep(held);
                let released = Instant::now();
                drop(operation);
                released
            });
            startup.wait_for_service(|| false).unwrap();
            let woke = Instant::now();
            let released = operation.join().unwrap();
            assert!(
                woke.saturating_duration_since(released) < Duration::from_secs(1),
                "the wait ends when the operation does"
            );
            ready(startup.run(no_provision, || false).unwrap());
            assert_eq!(supervisor.starts.load(Ordering::Relaxed), 0);
        }

        #[test]
        fn waiting_for_a_service_operation_can_cancel_while_its_lock_is_held() {
            let fixture = Fixture::new();
            let data = Directory::open(&fixture.paths.data, false).unwrap();
            let _operation = data.lock("service.lock", false).unwrap();
            let supervisor = AccountSupervisor::new(fixture.paths.clone());
            let checks = AtomicUsize::new(0);
            assert_eq!(
                LocalStartup::with_service(supervisor.service())
                    .unwrap()
                    .wait_for_service(|| checks.fetch_add(1, Ordering::Relaxed) >= 3),
                Ok(())
            );
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
                                ready(launch_settled(
                                    &LocalStartup::with_service(service.clone()).unwrap(),
                                    || {
                                        provisions.fetch_add(1, Ordering::Relaxed);
                                        provision(service, archive, signature)
                                    },
                                ));
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
                            automatic_start: true,
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
                let mut wire = protocol::Wire::new(&mut stream);
                let hello: serde_json::Value = wire.read()?;
                wire.write(&serde_json::json!({"selected":0, "build":hello["build"], "server":hello["server"], "server_major":protocol::MAJOR, "client":hello["client"]}))
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
                &serde_json::json!({"format":"account-local-2", "server":[1]}),
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
                    automatic_start: true,
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
            let sibling = fixture.root.join("spaceterm");
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
