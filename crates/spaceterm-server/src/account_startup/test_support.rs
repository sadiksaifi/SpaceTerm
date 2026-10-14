//! Isolated account-server process composition. Never compiled into production builds.

use super::{AccountStartup, RecoveredAccountServer, StartupFailure};
use crate::adapters::terminal::AccountLaunchFacts;
use spaceterm_core::{
    authority::{ServerEpoch, SessionKey},
    native_management::{NativeManagementRequestRef, NativeRetirementAction},
    retirement::SessionCleanupObservation,
};
use spaceterm_host::{
    account_service::{
        LingerState, RegisteredServer, RuntimeAssessment, ServiceClock, ServiceFailure,
        ServiceFuture, ServiceManager, ServiceProcesses, ServiceSupervisor, ServiceTime,
        SupervisorState, TrustedAccountFacts, test_support::AccountServiceFixture,
    },
    shell_launch::ShellLaunchPlanner,
    startup::BuildIdentity,
};
use spaceterm_terminal::metadata::LocalMachine;
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

struct Manager(Mutex<SupervisorState>);
impl ServiceManager for Manager {
    fn status<'a>(&'a self, _: Option<&'a RegisteredServer>) -> ServiceFuture<'a, SupervisorState> {
        Box::pin(async {
            Ok(*self
                .0
                .lock()
                .map_err(|_| ServiceFailure::SupervisorUnavailable)?)
        })
    }
    fn scoped_processes<'a>(&'a self, _: &'a RegisteredServer) -> ServiceFuture<'a, Vec<u32>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn linger(&self) -> ServiceFuture<'_, Option<LingerState>> {
        Box::pin(async { Ok(Some(LingerState::Enabled)) })
    }
    fn configure<'a>(&'a self, _: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn start<'a>(&'a self, _: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async {
            *self
                .0
                .lock()
                .map_err(|_| ServiceFailure::SupervisorUnavailable)? = SupervisorState::Running;
            Ok(())
        })
    }
    fn stop<'a>(&'a self, _: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async {
            *self
                .0
                .lock()
                .map_err(|_| ServiceFailure::SupervisorUnavailable)? = SupervisorState::Inactive;
            Ok(())
        })
    }
    fn uninstall<'a>(&'a self, _: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn verify_installation(&self, _: &RegisteredServer) -> Result<(), ServiceFailure> {
        Ok(())
    }
}
struct Clock;
impl ServiceClock for Clock {
    fn now(&self) -> Result<ServiceTime, ServiceFailure> {
        Ok(ServiceTime {
            boot: [1; 32],
            millis: 1,
        })
    }
}

/// The test owns this account root and supplies native process evidence explicitly.
/// Keys, registration, SQLite, actor, PTY and transports remain the real providers.
pub fn supervisor(
    root: &Path,
    processes: Arc<dyn ServiceProcesses>,
) -> Result<Arc<ServiceSupervisor>, StartupFailure> {
    let fixture = AccountServiceFixture::new(root, BuildIdentity::Development)
        .map_err(StartupFailure::Account)?;
    Ok(Arc::new(fixture.with_providers(
        Arc::new(Manager(Mutex::new(SupervisorState::Running))),
        processes,
        Arc::new(Clock),
        None,
    )))
}

struct FixtureInstallations;
impl spaceterm_host::account_service::ServiceInstallations for FixtureInstallations {
    fn approved_executable(
        &self,
        _: &RegisteredServer,
    ) -> Result<std::path::PathBuf, ServiceFailure> {
        std::env::current_exe().map_err(|_| ServiceFailure::StorageUnavailable)
    }
    fn uninstall(&self, _: &RegisteredServer) -> Result<(), ServiceFailure> {
        Ok(())
    }
}

/// Uses the production signed custody and native process provider. Only the
/// service-manager scope and executable selection are supplied by the fixture.
pub fn retained_supervisor(root: &Path) -> Result<Arc<ServiceSupervisor>, StartupFailure> {
    retained_supervisor_with_clock(root, None)
}

/// Injects only the native boot-clock observation for reboot acceptance.
pub fn retained_supervisor_with_clock(
    root: &Path,
    clock: Option<Arc<dyn ServiceClock>>,
) -> Result<Arc<ServiceSupervisor>, StartupFailure> {
    use spaceterm_host::account_service::{
        NativeServiceProcesses, runtime_inventory::RetainedRuntimeProcesses,
    };
    let fixture = AccountServiceFixture::new(root, BuildIdentity::Development)
        .map_err(StartupFailure::Account)?;
    let manager: Arc<dyn ServiceManager> = Arc::new(Manager(Mutex::new(SupervisorState::Running)));
    let processes = NativeServiceProcesses::current(Arc::new(FixtureInstallations))
        .map_err(StartupFailure::Account)?
        .with_manager(manager.clone())
        .with_runtime(Arc::new(RetainedRuntimeProcesses));
    let processes = match clock {
        Some(clock) => processes.with_clock(clock),
        None => processes,
    };
    Ok(Arc::new(fixture.with_providers(
        manager,
        Arc::new(processes),
        Arc::new(Clock),
        None,
    )))
}

/// Startup-only test absence provider. It refuses every retained old Session.
/// The parent acceptance test supplies runtime-owned assessments for native Stop.
pub struct StartupAbsence;
impl ServiceProcesses for StartupAbsence {
    fn assess<'a>(&'a self, _: &'a RegisteredServer) -> ServiceFuture<'a, RuntimeAssessment> {
        Box::pin(async { Err(ServiceFailure::CleanupUnresolved) })
    }
    fn revalidate<'a>(
        &'a self,
        _: &'a RegisteredServer,
        _: &'a RuntimeAssessment,
    ) -> ServiceFuture<'a, ()> {
        Box::pin(async { Err(ServiceFailure::CleanupUnresolved) })
    }
    fn abort_reservation<'a>(
        &'a self,
        _: &'a RegisteredServer,
        _: &'a RuntimeAssessment,
    ) -> ServiceFuture<'a, ()> {
        Box::pin(async { Err(ServiceFailure::CleanupUnresolved) })
    }
    fn quiesce<'a>(
        &'a self,
        _: &'a RegisteredServer,
        _: NativeManagementRequestRef,
        _: NativeRetirementAction,
        _: &'a RuntimeAssessment,
    ) -> ServiceFuture<'a, Vec<SessionCleanupObservation>> {
        Box::pin(async { Err(ServiceFailure::CleanupUnresolved) })
    }
    fn verify_absent<'a>(&'a self, registration: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            match tokio::net::UnixStream::connect(registration.endpoint()?.path()).await {
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                    ) =>
                {
                    Ok(())
                }
                _ => Err(ServiceFailure::CleanupUnresolved),
            }
        })
    }
    fn previous_instance_cleanup<'a>(
        &'a self,
        registration: &'a RegisteredServer,
        _: ServerEpoch,
        sessions: &'a [SessionKey],
    ) -> ServiceFuture<'a, Vec<SessionCleanupObservation>> {
        Box::pin(async move {
            if !sessions.is_empty() {
                return Err(ServiceFailure::CleanupUnresolved);
            }
            self.verify_absent(registration).await?;
            Ok(Vec::new())
        })
    }
}

/// Runs the same server composition against an existing fixture registration.
/// This does not initialize a key, organization store or service configuration.
pub async fn run(root: &Path) -> Result<crate::shutdown::ShutdownReport, StartupFailure> {
    let root = root
        .canonicalize()
        .map_err(|_| StartupFailure::Account(ServiceFailure::StorageUnavailable))?;
    let facts = TrustedAccountFacts::current().map_err(StartupFailure::Account)?;
    let supervisor = retained_supervisor(&root)?;
    let startup = AccountStartup::admit(&supervisor)?;
    let launch = AccountLaunchFacts {
        uid: facts.uid(),
        home: root.clone(),
        planner: ShellLaunchPlanner::for_test(root.join("shell"), root.clone()),
        machine: LocalMachine::default(),
    };
    let mut terminate =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .map_err(|_| StartupFailure::Account(ServiceFailure::SupervisorUnavailable))?;
    let mut interrupt =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
            .map_err(|_| StartupFailure::Account(ServiceFailure::SupervisorUnavailable))?;
    RecoveredAccountServer::recover(supervisor, startup, launch)
        .await?
        .run_until(async move {
            tokio::select! { _ = terminate.recv() => {}, _ = interrupt.recv() => {} }
        })
        .await
}
