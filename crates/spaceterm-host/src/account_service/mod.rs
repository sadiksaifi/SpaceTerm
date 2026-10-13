//! Account-owned service lifecycle and durable original-request authority.
//!
//! The native manager and runtime producers are selected at composition. Read-only
//! discovery and owner validation never initialize storage or issue authority.

mod commands;
mod launchd;
mod linger;
mod owner_enrollment;
mod processes;
mod systemd;

pub use commands::{
    NativeServiceCommandRunner, ServiceCommand, ServiceCommandOutput, ServiceCommandRunner,
    ServiceInstallations,
};
pub use launchd::LaunchdServiceManager;
pub use linger::{
    LingerAuthenticationPrompt, PkexecLingerAuthorization, ScopedLingerAuthentication,
    SshLingerAuthorization, SshLingerExchange, SudoLingerAuthorization,
};
pub use systemd::SystemdServiceManager;
mod registration;
mod supervisor;

pub use processes::{
    RuntimeAssessment, ServiceClock, ServiceManager, ServiceProcesses, ServiceTime,
};
pub use registration::RegisteredServer;
pub use supervisor::{AccountStartup, AccountStartupAdmission, ServiceSupervisor, StorageWait};

use crate::{
    native_deployment::contracts::NativeRequestId,
    startup::{AccountBuildKey, DesiredServerState, InstallationId},
};
use serde::{Deserialize, Serialize};
use spaceterm_core::operations::RequestDigest;
use std::{future::Future, pin::Pin, sync::Arc};

/// A service-owned operation observer. Dropping it does not cancel accepted work.
pub type ServiceFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, ServiceFailure>> + Send + 'a>>;

/// Independently observed account linger.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LingerState {
    /// The account user manager survives logout and starts at boot.
    Enabled,
    /// The account requires the fixed administrator-authorized linger operation.
    Disabled,
    /// Linger could not be inspected.
    Unavailable,
}
/// Fixed authorization request for the actual resolved account.
pub struct LingerRequest {
    pub(super) uid: u32,
}
impl LingerRequest {
    /// Returns the independently resolved nonroot account.
    pub fn account_uid(&self) -> u32 {
        self.uid
    }
}
/// Authorization for enabling linger only, never a SpaceTerm executable.
pub trait LingerAuthorization: Send + Sync {
    /// Enables linger for the fixed resolved account.
    fn enable_linger<'a>(&'a self, request: &'a LingerRequest) -> ServiceFuture<'a, ()>;
}
/// Fixed local sudo interaction policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SudoMode {
    /// Uses the controlling terminal for authorization.
    Interactive,
    /// Fails without prompting when authorization is unavailable.
    NonInteractive,
}
/// Native supervision observation, separate from deliberate intent and authentication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SupervisorState {
    /// This service has no installed configuration.
    NotInstalled,
    /// Automatic supervision is disabled.
    Disabled,
    /// The service is installed but inactive.
    Inactive,
    /// Startup is underway.
    Starting,
    /// The manager observes a running service.
    Running,
    /// Graceful shutdown is underway.
    Stopping,
    /// The OS has suppressed supervision.
    Suppressed,
}
/// Exact artifact selection facts. The artifact provider can reexport this one DTO.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallationStatus {
    /// Logical installed version identity.
    pub installation: InstallationId,
    /// Signed build version.
    pub version: spaceterm_distribution::BuildVersion,
    /// Digest of the authenticated archive bytes.
    pub artifact_sha256: [u8; 32],
}
/// Current native facts, not the historical result of a lifecycle request.
pub struct ServiceStatus {
    /// Actual account and compiled identity.
    pub key: AccountBuildKey,
    /// Registered public server identity, if installed.
    pub server: Option<spaceterm_core::identity::ServerId>,
    /// Deliberately retained intent.
    pub desired: Option<DesiredServerState>,
    /// Independently observed manager state.
    pub supervisor: SupervisorState,
    /// Linux linger, absent on macOS.
    pub linger: Option<LingerState>,
    /// Exact approved code, never selected from a directory scan.
    pub approved: Option<InstallationStatus>,
    /// A staged version without startup authority.
    pub staged: Option<InstallationStatus>,
    /// Original accepted unresolved request, never an ID allocator.
    pub pending: Option<NativeRequestId>,
}
/// Foreground destructive lifecycle action.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ServiceAction {
    /// Deliberately stops this server.
    Stop,
    /// Restarts only Running approved code.
    Restart,
    /// Removes service configuration and verified code, retaining data.
    Uninstall,
    /// Selects one exact staged version.
    Activate(InstallationId),
}
/// Durable original lifecycle phase.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ServicePhase {
    /// Provider-issued request or retained assessment without accepted effects.
    Reserved,
    /// Exact original contents are durably admitted.
    Accepted,
    /// Deliberate intent or action-specific barrier is durable.
    IntentCommitted,
    /// Exact runtime retirement is underway.
    Quiescing,
    /// Fixed configuration or approved selection is being published.
    Publishing,
    /// Original startup is being reconciled.
    Starting,
    /// Immutable original outcome is retained.
    Completed,
}
/// Immutable historical service outcome.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ServiceOutcome {
    /// Explicit installation, possibly reuse without a Start.
    Installed {
        /// Registered public pin.
        #[serde(with = "supervisor::pin_codec")]
        server: spaceterm_protocol::authentication::ServerPin,
        /// Exact approved artifact.
        installation: InstallationStatus,
        /// An established registration was reused.
        reused: bool,
    },
    /// A verified version was staged without activation.
    Staged(InstallationStatus),
    /// Explicit approved startup completed.
    Started,
    /// Durable Stop and exact cleanup completed.
    Stopped,
    /// Running-only approved restart completed.
    Restarted,
    /// Exact staged approval completed without changing Stopped.
    Activated(InstallationStatus),
    /// Configuration and verified code were removed, retaining private data.
    Uninstalled,
}
/// Resolution of the original retained request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ServiceRequestState {
    /// Accepted work has not finished.
    Pending,
    /// The original immutable result.
    Completed(ServiceOutcome),
    /// Admission or pre-effect validation failed.
    NotApplied(ServiceFailure),
    /// Original publication durability is uncertain.
    Indeterminate,
}
/// Bounded content-free progress retaining caller identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceProgress {
    /// The ID retained before submission.
    pub request: NativeRequestId,
    /// The canonical original contents, not a current status digest.
    pub request_digest: RequestDigest,
    /// Original durable phase.
    pub phase: ServicePhase,
    /// Original resolution.
    pub state: ServiceRequestState,
}
/// Original all-client assessment. Its private fields cannot be decoded from a wire request.
pub struct ServiceAssessment {
    request: NativeRequestId,
    record: supervisor::AssessmentRecord,
    clock: Arc<dyn ServiceClock>,
}
impl ServiceAssessment {
    /// Returns the original request.
    pub fn request(&self) -> NativeRequestId {
        self.request
    }
    /// Returns the exact assessed action.
    pub fn action(&self) -> ServiceAction {
        self.record.action
    }
    /// Returns the restore comparison token, not mutation authority.
    pub fn token(&self) -> [u8; 32] {
        self.record.token
    }
    /// Returns the runtime-owned affected count.
    pub fn affected_sessions(&self) -> u32 {
        self.record.runtime.sessions.len() as u32
    }
    /// Confirms this exact still-valid assessment without administrator authority.
    pub fn confirm(self) -> Result<ConfirmedServiceAction, ServiceFailure> {
        self.record.check_time(self.clock.now()?)?;
        Ok(ConfirmedServiceAction(self))
    }
}
/// Noncloneable explicit confirmation of an original assessment.
pub struct ConfirmedServiceAction(pub(super) ServiceAssessment);
impl ConfirmedServiceAction {
    /// Returns the original request, never a newly allocated replacement.
    pub fn request(&self) -> NativeRequestId {
        self.0.request
    }
}

/// Content-free account service failures. No utility output or paths are retained.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ServiceFailure {
    /// The current effective account is root.
    RootAccount,
    /// Native account services are not supported on this host.
    UnsupportedHost,
    /// Required established storage is absent.
    Missing,
    /// Native storage is locked.
    StorageLocked,
    /// Native storage cannot be read or synchronized.
    StorageUnavailable,
    /// Ownership, permissions, entry type or descriptor identity is unsafe.
    UnsafeStorage,
    /// A bounded record failed structural validation.
    InvalidRecord,
    /// A persisted format is unsupported.
    UnsupportedSchema,
    /// The retained account, build or server differs.
    IdentityMismatch,
    /// The signed artifact was rejected.
    ArtifactRejected,
    /// Organization storage is not compatible with the selected code.
    IncompatibleStorage,
    /// The identifier was never reserved by this provider.
    UnknownRequest,
    /// An existing request has different canonical contents.
    RequestConflict,
    /// The identifier has been permanently retired.
    RequestRetired,
    /// A runtime assessment changed or expired.
    StaleAssessment,
    /// Explicit foreground confirmation is required.
    ConfirmationRequired,
    /// Linger authorization was denied.
    AuthorizationDenied,
    /// Linger authorization was cancelled.
    AuthorizationCancelled,
    /// The fixed linger authorization adapter is unavailable.
    AuthorizationUnavailable,
    /// The native service manager is unavailable.
    SupervisorUnavailable,
    /// macOS has no GUI login domain for this account.
    LoginSessionUnavailable,
    /// Native supervision is suppressed.
    Suppressed,
    /// Admission or record size exceeds its ceiling.
    LimitExceeded,
    /// Original accepted work has not completed.
    Pending,
    /// Exact previous-runtime cleanup has not been proved.
    CleanupUnresolved,
    /// A published effect's durability is uncertain.
    Indeterminate,
}

impl From<crate::private_filesystem::Failure> for ServiceFailure {
    fn from(failure: crate::private_filesystem::Failure) -> Self {
        use crate::private_filesystem::Failure;
        match failure {
            Failure::Missing => Self::Missing,
            Failure::AlreadyExists | Failure::Conflict => Self::RequestConflict,
            Failure::LimitExceeded => Self::LimitExceeded,
            Failure::Unsafe => Self::UnsafeStorage,
            Failure::Unavailable => Self::StorageUnavailable,
            Failure::Indeterminate => Self::Indeterminate,
            Failure::Busy => Self::Pending,
        }
    }
}

#[cfg(feature = "test-support")]
#[doc(hidden)]
pub mod test_support {
    //! Controlled filesystem/account boundary for public-interface integration tests.
    //!
    //! These constructors are compiled only with test-support. They do not supply a
    //! production install operation, artifact verification or native supervision.

    pub use super::commands::test_support as native_managers;

    use std::{
        path::{Path, PathBuf},
        sync::atomic::AtomicBool,
    };

    use super::{RegisteredServer, ServiceFailure, ServiceSupervisor};
    use crate::{
        private_filesystem::Directory,
        server_keys::{IdentityCreatePermit, ServerKeyStore},
        startup::{AccountBuildKey, BuildIdentity, DesiredServerState},
    };

    /// An isolated native filesystem fixture, never the actual account's service anchor.
    pub struct AccountServiceFixture {
        anchor: PathBuf,
        data: PathBuf,
        runtime: PathBuf,
        key: AccountBuildKey,
    }
    impl AccountServiceFixture {
        /// Resolves isolated paths below an existing test root with the actual UID.
        pub fn new(root: &Path, build: BuildIdentity) -> Result<Self, ServiceFailure> {
            let root = root
                .canonicalize()
                .map_err(|_| ServiceFailure::StorageUnavailable)?;
            // SAFETY: geteuid is a side-effect-free credential query.
            let uid = unsafe { libc::geteuid() };
            if uid == 0 {
                return Err(ServiceFailure::RootAccount);
            }
            let name = match build {
                BuildIdentity::Release => "release",
                BuildIdentity::Preflight => "preflight",
                BuildIdentity::Development => "development",
            };
            Ok(Self {
                anchor: root.join(name).join("authority"),
                data: root.join(name).join("data"),
                runtime: root.join(name).join("runtime"),
                key: AccountBuildKey {
                    account_uid: uid,
                    build_identity: build,
                },
            })
        }
        /// Reconstructs the real provider against this isolated account/filesystem boundary.
        pub fn reopen(&self) -> ServiceSupervisor {
            ServiceSupervisor {
                key: self.key,
                anchor: self.anchor.clone(),
                indeterminate: std::sync::Arc::new(AtomicBool::new(false)),
                authorization: None,
                manager: None,
                processes: None,
                clock: std::sync::Arc::new(super::processes::NativeClock),
            }
        }
        /// Injects native manager, runtime and clock boundaries into the real lifecycle core.
        pub fn with_providers(
            &self,
            manager: std::sync::Arc<dyn super::ServiceManager>,
            processes: std::sync::Arc<dyn super::ServiceProcesses>,
            clock: std::sync::Arc<dyn super::ServiceClock>,
            authorization: Option<std::sync::Arc<dyn super::LingerAuthorization>>,
        ) -> ServiceSupervisor {
            let mut supervisor = self.reopen();
            supervisor.manager = Some(manager);
            supervisor.processes = Some(processes);
            supervisor.clock = clock;
            supervisor.authorization = authorization;
            supervisor
        }
        /// Supplies an internal initialization scope for exclusive identity tests.
        pub fn identity_scope(
            &self,
        ) -> Result<(ServerKeyStore, IdentityCreatePermit), ServiceFailure> {
            let directory = Directory::create(&self.data)?;
            Ok(ServerKeyStore::initial(directory, self.key))
        }
        /// Publishes test registration through the real exclusive private-file mechanics.
        /// It reuses established registration without changing desired state.
        pub fn initialize(
            &self,
            desired: DesiredServerState,
        ) -> Result<RegisteredServer, ServiceFailure> {
            self.initialize_with_installation(desired, None)
        }
        /// Uses the real initial-install core with artifact metadata supplied by a test provider.
        pub fn initialize_with_installation(
            &self,
            desired: DesiredServerState,
            approved: Option<super::InstallationStatus>,
        ) -> Result<RegisteredServer, ServiceFailure> {
            let supervisor = self.reopen();
            if let Some(registration) = supervisor.discover()? {
                return Ok(registration);
            }
            let anchor = Directory::create(&self.anchor)?;
            let lease = anchor.lease("service-lock")?;
            supervisor.initialize_registration(
                &lease,
                self.data.clone(),
                self.runtime.clone(),
                desired,
                approved,
            )
        }
        /// Publishes a separately staged artifact through real registration mechanics.
        pub fn stage_installation(
            &self,
            status: super::InstallationStatus,
        ) -> Result<(), ServiceFailure> {
            let supervisor = self.reopen();
            let anchor = Directory::open(&self.anchor)?.ok_or(ServiceFailure::Missing)?;
            let lease = anchor.lease("service-lock")?;
            let (directory, snapshot, mut registration) = supervisor
                .read_registration()?
                .ok_or(ServiceFailure::Missing)?;
            registration.staged = Some(status);
            registration.generation = registration
                .generation
                .checked_add(1)
                .ok_or(ServiceFailure::LimitExceeded)?;
            lease.verify()?;
            supervisor.publish_registration(&registration, &snapshot, &directory)
        }
        /// Explicitly reconciles original directory admission without creating missing children.
        pub fn reconcile_initialization(&self) -> Result<(), ServiceFailure> {
            Directory::reconcile_initialization(&self.anchor).map_err(Into::into)
        }
        /// Returns the isolated fixed anchor for native replacement and malformed-file tests.
        pub fn anchor(&self) -> &Path {
            &self.anchor
        }
        /// Returns the isolated private data directory for native file adversaries.
        pub fn data_directory(&self) -> &Path {
            &self.data
        }
    }
    thread_local! {
        static PHASE_HOOK: std::cell::RefCell<Option<PhaseHook>> = const { std::cell::RefCell::new(None) };
    }
    struct PhaseHook {
        phase: super::ServicePhase,
        operation: Box<dyn FnOnce() -> Result<(), ServiceFailure>>,
    }
    /// Injects process interruption after an exact durable lifecycle phase.
    pub fn interrupt_after_phase(phase: super::ServicePhase) {
        after_next_phase(phase, || Err(ServiceFailure::SupervisorUnavailable));
    }
    /// Runs an I/O or process fault at a durable lifecycle boundary in test composition.
    pub fn after_next_phase(
        phase: super::ServicePhase,
        operation: impl FnOnce() -> Result<(), ServiceFailure> + 'static,
    ) {
        PHASE_HOOK.with(|slot| {
            assert!(slot.borrow().is_none(), "phase hook already armed");
            *slot.borrow_mut() = Some(PhaseHook {
                phase,
                operation: Box::new(operation),
            });
        });
    }
    pub(super) fn after_phase(phase: super::ServicePhase) -> Result<(), ServiceFailure> {
        let hook = PHASE_HOOK.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.as_ref().is_some_and(|hook| hook.phase == phase) {
                slot.take()
            } else {
                None
            }
        });
        if let Some(hook) = hook {
            (hook.operation)()?;
        }
        Ok(())
    }
    /// Interrupts publication after durable Pending but before target publication.
    pub fn fail_next_publication_before_effect() {
        crate::private_filesystem::test_support::fail_next_publication_before_effect();
    }

    /// Supplies the native committed-but-unsynced outcome for the next publication.
    /// This is a commit-result fault, not a power-loss or filesystem fault simulation.
    pub fn fail_next_publication_durability() {
        crate::private_filesystem::test_support::fail_next_publication_durability();
    }

    /// Interrupts original directory initialization before mkdir.
    pub fn fail_next_creation_before_effect() {
        crate::private_filesystem::test_support::fail_next_creation_before_effect();
    }

    /// Injects EIO before synchronization of a newly created directory's parent.
    pub fn fail_next_creation_parent_sync() {
        crate::private_filesystem::test_support::fail_next_creation_parent_sync();
    }

    /// Injects EIO at the fixed phase record's native file synchronization.
    pub fn fail_next_mutation_completion_sync() {
        crate::private_filesystem::test_support::fail_next_mutation_completion_sync();
    }

    /// Injects completion-sync EIO followed by rollback-write EIO.
    pub fn fail_next_completion_and_rollback_write() {
        crate::private_filesystem::test_support::fail_next_completion_and_rollback_write();
    }

    /// Pauses native directory admission at its pathname-open boundary.
    pub fn before_next_directory_open(path: &Path, operation: impl FnOnce() + 'static) {
        crate::private_filesystem::test_support::before_next_directory_open(path, operation);
    }
}

impl From<crate::server_keys::KeyStoreFailure> for ServiceFailure {
    fn from(error: crate::server_keys::KeyStoreFailure) -> Self {
        use crate::server_keys::KeyStoreFailure;
        match error {
            KeyStoreFailure::Missing => Self::Missing,
            KeyStoreFailure::StorageLocked => Self::StorageLocked,
            KeyStoreFailure::StorageUnavailable => Self::StorageUnavailable,
            KeyStoreFailure::UnsafeStorage => Self::UnsafeStorage,
            KeyStoreFailure::InvalidRecord => Self::InvalidRecord,
            KeyStoreFailure::IdentityMismatch => Self::IdentityMismatch,
            KeyStoreFailure::AlreadyExists | KeyStoreFailure::Conflict => Self::RequestConflict,
            KeyStoreFailure::LimitExceeded => Self::LimitExceeded,
            KeyStoreFailure::Indeterminate => Self::Indeterminate,
        }
    }
}
