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
    fn approved(&self, path: &Path) -> Result<Vec<ServiceProcessIdentity>, ServiceFailure> {
        let mut found = Vec::new();
        for pid in native::pids(self.uid)? {
            if let Some(process) = self.observe(pid)?
                && (process.executable == path
                    || process
                        .executable
                        .as_os_str()
                        .as_encoded_bytes()
                        .strip_suffix(b" (deleted)")
                        == Some(path.as_os_str().as_encoded_bytes()))
            {
                // A system agent with saved privileges says nothing about this code.
                // Reject mixed credentials only when its executable actually matches.
                if !process.fully_account_owned {
                    return Err(ServiceFailure::CleanupUnresolved);
                }
                found.push(process);
            }
        }
        Ok(found)
    }
}

/// Native absence observations for account services.
/// A live runtime or old Session requires runtime-owned custody and is never
/// declared clean merely because its main PID disappeared.
pub struct NativeServiceProcesses {
    table: NativeServiceProcessTable,
    installations: Arc<dyn ServiceInstallations>,
    runtime: Option<Arc<dyn ServiceProcesses>>,
}
impl NativeServiceProcesses {
    /// Composes actual-account native observations with signed executable selection.
    pub fn current(installations: Arc<dyn ServiceInstallations>) -> Result<Self, ServiceFailure> {
        Ok(Self {
            table: NativeServiceProcessTable::current()?,
            installations,
            runtime: None,
        })
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
    fn absent(&self, registration: &RegisteredServer, startup: bool) -> Result<(), ServiceFailure> {
        if registration.key().account_uid != self.table.uid {
            return Err(ServiceFailure::IdentityMismatch);
        }
        let executable = self.installations.approved_executable(registration)?;
        // Startup runs inside the approved executable. Its own PID is not a previous instance.
        if self
            .table
            .approved(&executable)?
            .iter()
            .any(|process| !startup || process.pid != std::process::id())
        {
            return Err(ServiceFailure::CleanupUnresolved);
        }
        let endpoint = registration
            .endpoint()
            .map_err(|_| ServiceFailure::CleanupUnresolved)?;
        match endpoint.prove_no_listener() {
            Ok(()) => Ok(()),
            Err(_) => Err(ServiceFailure::CleanupUnresolved),
        }
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
                self.absent(registration, false)?;
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
            self.absent(registration, false)
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
                self.absent(registration, true)?;
                return Ok(observations);
            }
            // Only a never-started store can use native root/listener absence alone.
            // An old epoch may own Git or other descendants even with no Sessions.
            if epoch.get() != 0 || !sessions.is_empty() {
                return Err(ServiceFailure::CleanupUnresolved);
            }
            self.absent(registration, true)?;
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
