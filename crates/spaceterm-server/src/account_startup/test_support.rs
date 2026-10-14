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
    run_with_home(root, root).await
}

/// Supplies the private display harness's HOME so its existing process retirement scope
/// includes every shell descendant. Storage remains under the separately admitted root.
pub async fn run_with_home(
    root: &Path,
    home: &Path,
) -> Result<crate::shutdown::ShutdownReport, StartupFailure> {
    let home = home
        .canonicalize()
        .map_err(|_| StartupFailure::Account(ServiceFailure::StorageUnavailable))?;
    let root = root
        .canonicalize()
        .map_err(|_| StartupFailure::Account(ServiceFailure::StorageUnavailable))?;
    let facts = TrustedAccountFacts::current().map_err(StartupFailure::Account)?;
    let supervisor = supervisor(&root, Arc::new(StartupAbsence))?;
    let startup = AccountStartup::admit(&supervisor)?;
    let launch = AccountLaunchFacts {
        uid: facts.uid(),
        home,
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

/// Initializes a fresh accessibility fixture with real private keys and SQLite storage.
/// Only test-enabled binaries expose this operation; existing roots are never reset.
pub fn initialize(root: &Path) -> Result<(), StartupFailure> {
    use spaceterm_core::{identity::ServerId, organization::ServerDirectory};
    use spaceterm_host::{
        organization_store::{
            SqliteOrganizationStore, StoreIdentity, StoreLimits,
            bootstrap::{InitializationDisposition, NewServerOrigin, NewStoreSeed},
            migrations, schema,
        },
        startup::DesiredServerState,
        storage::Database,
    };
    use std::{io::Write as _, os::unix::fs::OpenOptionsExt as _};
    let root = root
        .canonicalize()
        .map_err(|_| StartupFailure::Account(ServiceFailure::StorageUnavailable))?;
    let fixture = AccountServiceFixture::new(&root, BuildIdentity::Development)
        .map_err(StartupFailure::Account)?;
    if fixture
        .reopen()
        .discover()
        .map_err(StartupFailure::Account)?
        .is_some()
    {
        return Err(StartupFailure::Account(ServiceFailure::IdentityMismatch));
    }
    let mut shell = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o700)
        .open(root.join("shell"))
        .map_err(|_| StartupFailure::Account(ServiceFailure::StorageUnavailable))?;
    shell
        .write_all(b"#!/bin/sh\nexec /bin/bash --noprofile --norc \"$@\"\n")
        .and_then(|_| shell.sync_all())
        .map_err(|_| StartupFailure::Account(ServiceFailure::StorageUnavailable))?;
    let registration = fixture
        .initialize_with_installation(
            DesiredServerState::Running,
            Some(spaceterm_host::account_service::InstallationStatus {
                installation: spaceterm_host::startup::InstallationId(std::num::NonZeroU64::MIN),
                version: crate::distribution_metadata::version()
                    .map_err(StartupFailure::Distribution)?,
                artifact_sha256: [1; 32],
            }),
        )
        .map_err(StartupFailure::Account)?;
    let pin = registration.server_pin();
    let identity = StoreIdentity {
        server: ServerId::new(pin.identity),
        account: registration.key(),
        server_public_key: pin.public_key,
    };
    let home = ServerDirectory::new(
        identity.server,
        root.as_os_str().as_encoded_bytes().to_vec(),
    )
    .map_err(|_| StartupFailure::Account(ServiceFailure::UnsafeStorage))?;
    let database = Database::open_for_application(
        registration.organization_directory(),
        migrations(),
        schema::APPLICATION_ID,
    )
    .map_err(StartupFailure::Storage)?;
    let services = crate::adapters::store::production_services(identity, home)
        .map_err(StartupFailure::Store)?;
    match SqliteOrganizationStore::initialize_new(
        database,
        NewStoreSeed {
            identity,
            origin: NewServerOrigin::LocalHeadless,
        },
        StoreLimits::production(),
        services,
    ) {
        InitializationDisposition::Durable { .. } => Ok(()),
        InitializationDisposition::NotApplied(failure) => Err(StartupFailure::Store(failure)),
        InitializationDisposition::Indeterminate => {
            Err(StartupFailure::Account(ServiceFailure::Indeterminate))
        }
    }
}

#[cfg(test)]
mod fixture_tests {
    use super::*;

    #[test]
    fn accessibility_fixture_initializes_real_account_store_and_refuses_reinitialization() {
        #[cfg(target_os = "linux")]
        let parent = "/var/tmp";
        #[cfg(target_os = "macos")]
        let parent = "/private/tmp";
        let root = tempfile::tempdir_in(parent).unwrap();
        initialize(root.path()).unwrap();
        let supervisor = supervisor(root.path(), Arc::new(StartupAbsence)).unwrap();
        let registration = supervisor.discover().unwrap().unwrap();
        let pin = registration.server_pin();
        // Opening SQLite alone is insufficient: the account startup also requires an approved installation.
        drop(AccountStartup::admit(&supervisor).unwrap());
        let home = spaceterm_core::organization::ServerDirectory::new(
            spaceterm_core::identity::ServerId::new(pin.identity),
            root.path().as_os_str().as_encoded_bytes().to_vec(),
        )
        .unwrap();
        AccountStartup::open(registration)
            .unwrap()
            .open_store(home)
            .unwrap();
        assert!(root.path().join("shell").is_file());
        assert!(initialize(root.path()).is_err());
        assert_eq!(supervisor.discover().unwrap().unwrap().server_pin(), pin);
    }
}
