use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use super::{
    RegisteredServer, RuntimeAssessment, ServiceFailure, ServiceFuture, ServiceInstallations,
    ServiceProcesses,
};
use spaceterm_core::{
    authority::{ServerEpoch, SessionKey},
    native_management::{NativeManagementRequestRef, NativeRetirementAction},
    retirement::SessionCleanupObservation,
};

/// Exact native process identity; paths are authority inputs, never diagnostics.
#[derive(Clone, Eq, PartialEq)]
pub struct ServiceProcessIdentity {
    pid: u32,
    uid: u32,
    fully_account_owned: bool,
    start: u64,
    executable: PathBuf,
}
impl ServiceProcessIdentity {
    /// Returns the process ID, meaningful only together with its start identity.
    pub fn pid(&self) -> u32 {
        self.pid
    }
    /// Returns the independently observed account UID.
    pub fn uid(&self) -> u32 {
        self.uid
    }
    /// Returns the native per-boot process start identity.
    pub fn start_identity(&self) -> u64 {
        self.start
    }
    /// Returns the observed local executable path, never protocol file authority.
    pub fn executable(&self) -> &Path {
        &self.executable
    }
}
impl std::fmt::Debug for ServiceProcessIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ServiceProcessIdentity(..)")
    }
}

/// Native process observations restricted to the actual nonroot account.
/// This provider observes processes; it never signals a sampled PID.
pub struct NativeServiceProcessTable {
    uid: u32,
}
impl NativeServiceProcessTable {
    /// Resolves the account without using HOME or SHELL.
    pub fn current() -> Result<Self, ServiceFailure> {
        Ok(Self {
            uid: super::TrustedAccountFacts::current()?.uid(),
        })
    }
    /// Observes one process in this account, or returns None if it is absent or belongs to another account.
    pub fn observe(&self, pid: u32) -> Result<Option<ServiceProcessIdentity>, ServiceFailure> {
        if pid == 0 || pid > i32::MAX as u32 {
            return Err(ServiceFailure::InvalidRecord);
        }
        native::observe(self.uid, pid)
    }
}

/// Native absence observations for account services.
/// A live runtime or old Session requires runtime-owned custody and is never
/// declared clean merely because its main PID disappeared.
pub struct NativeServiceProcesses {
    table: NativeServiceProcessTable,
    installations: Arc<dyn ServiceInstallations>,
    manager: Arc<dyn super::ServiceManager>,
    runtime: Option<Arc<dyn ServiceProcesses>>,
}
impl NativeServiceProcesses {
    /// Composes actual-account native observations with signed executable selection.
    pub fn current(installations: Arc<dyn ServiceInstallations>) -> Result<Self, ServiceFailure> {
        #[cfg(target_os = "linux")]
        let manager = super::SystemdServiceManager::current(installations.clone())?;
        #[cfg(target_os = "macos")]
        let manager = super::LaunchdServiceManager::current(installations.clone())?;
        Ok(Self {
            table: NativeServiceProcessTable::current()?,
            installations,
            manager: Arc::new(manager),
            runtime: None,
        })
    }
    /// Uses the same manager scope provider as the lifecycle supervisor.
    pub fn with_manager(mut self, manager: Arc<dyn super::ServiceManager>) -> Self {
        self.manager = manager;
        self
    }
    /// Adds the owning actor's assessment, admission-barrier and descendant-custody port.
    /// An empty reconstructed registry must never supply previous-instance absence.
    pub fn with_runtime(mut self, runtime: Arc<dyn ServiceProcesses>) -> Self {
        self.runtime = Some(runtime);
        self
    }
    fn account(&self, registration: &RegisteredServer) -> Result<(), ServiceFailure> {
        if registration.key().account_uid != self.table.uid {
            return Err(ServiceFailure::IdentityMismatch);
        }
        registration.directory.verify().map_err(Into::into)
    }
    fn runtime(&self) -> Result<&dyn ServiceProcesses, ServiceFailure> {
        self.runtime
            .as_deref()
            .ok_or(ServiceFailure::CleanupUnresolved)
    }
    async fn absent(
        &self,
        registration: &RegisteredServer,
        startup: bool,
    ) -> Result<(), ServiceFailure> {
        if registration.key().account_uid != self.table.uid {
            return Err(ServiceFailure::IdentityMismatch);
        }
        let executable = self.installations.approved_executable(registration)?;
        let scope = self.manager.scoped_processes(registration).await?;
        if scope.len() > 65536 {
            return Err(ServiceFailure::LimitExceeded);
        }
        for pid in &scope {
            if let Some(process) = self.table.observe(*pid)? {
                // Every process retained in the service cgroup is in scope,
                // including descendants whose executable is not the server.
                // Startup exempts only this exact approved account-owned root.
                if !startup
                    || process.pid != std::process::id()
                    || !process.fully_account_owned
                    || process.executable != executable
                {
                    return Err(ServiceFailure::CleanupUnresolved);
                }
            } else {
                // None also covers another account or a vanished executable.
                // Only native process absence can discharge an in-scope PID.
                // SAFETY: signal zero observes existence without delivering a signal.
                if unsafe { libc::kill(*pid as i32, 0) } == 0
                    || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
                {
                    return Err(ServiceFailure::CleanupUnresolved);
                }
            }
        }
        if self.manager.scoped_processes(registration).await? != scope {
            return Err(ServiceFailure::CleanupUnresolved);
        }
        let endpoint = registration
            .endpoint()
            .map_err(|_| ServiceFailure::CleanupUnresolved)?;
        endpoint
            .prove_no_listener()
            .map_err(|_| ServiceFailure::CleanupUnresolved)
    }
}
impl ServiceProcesses for NativeServiceProcesses {
    fn assess<'a>(
        &'a self,
        registration: &'a RegisteredServer,
    ) -> ServiceFuture<'a, RuntimeAssessment> {
        Box::pin(async move {
            self.account(registration)?;
            let assessment = self.runtime()?.assess(registration).await?;
            assessment.validate(registration)?;
            if assessment.epoch.is_none() {
                self.absent(registration, false).await?;
            }
            Ok(assessment)
        })
    }
    fn revalidate<'a>(
        &'a self,
        registration: &'a RegisteredServer,
        assessment: &'a RuntimeAssessment,
    ) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            self.account(registration)?;
            assessment.validate(registration)?;
            self.runtime()?.revalidate(registration, assessment).await
        })
    }
    fn abort_reservation<'a>(
        &'a self,
        registration: &'a RegisteredServer,
        assessment: &'a RuntimeAssessment,
    ) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            self.account(registration)?;
            assessment.validate(registration)?;
            self.runtime()?
                .abort_reservation(registration, assessment)
                .await
        })
    }
    fn quiesce<'a>(
        &'a self,
        registration: &'a RegisteredServer,
        request: NativeManagementRequestRef,
        action: NativeRetirementAction,
        assessment: &'a RuntimeAssessment,
    ) -> ServiceFuture<'a, Vec<SessionCleanupObservation>> {
        Box::pin(async move {
            self.account(registration)?;
            assessment.validate(registration)?;
            let observations = self
                .runtime()?
                .quiesce(registration, request, action, assessment)
                .await?;
            super::processes::cleanup_matches(&assessment.sessions, &observations)?;
            Ok(observations)
        })
    }
    fn verify_absent<'a>(&'a self, registration: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            self.account(registration)?;
            // The runtime proves descendant custody; native observations prove
            // the approved root process and retained listener are also absent.
            self.runtime()?.verify_absent(registration).await?;
            self.absent(registration, false).await
        })
    }
    fn previous_instance_cleanup<'a>(
        &'a self,
        registration: &'a RegisteredServer,
        epoch: ServerEpoch,
        sessions: &'a [SessionKey],
    ) -> ServiceFuture<'a, Vec<SessionCleanupObservation>> {
        Box::pin(async move {
            self.account(registration)?;
            super::processes::validate_previous_scope(
                spaceterm_core::identity::ServerId::new(registration.server_pin().identity),
                epoch,
                sessions,
            )?;
            if let Some(runtime) = &self.runtime {
                let observations = runtime
                    .previous_instance_cleanup(registration, epoch, sessions)
                    .await?;
                super::processes::cleanup_matches(sessions, &observations)?;
                self.absent(registration, true).await?;
                return Ok(observations);
            }
            // Only a never-started store can use native root/listener absence alone.
            // An old epoch may own Git or other descendants even with no Sessions.
            if epoch.get() != 0 || !sessions.is_empty() {
                return Err(ServiceFailure::CleanupUnresolved);
            }
            self.absent(registration, true).await?;
            Ok(Vec::new())
        })
    }
}

#[cfg(target_os = "linux")]
#[path = "native_processes/linux.rs"]
mod native;
#[cfg(target_os = "macos")]
#[path = "native_processes/macos.rs"]
mod native;

#[cfg(target_os = "linux")]
pub(super) fn systemd_scope(main: u32, cgroup: &str) -> Result<Vec<u32>, ServiceFailure> {
    native::unit_scope(main, cgroup)
}
