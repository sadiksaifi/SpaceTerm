//! Established account startup opens retained keys and storage, never initializes
//! them. Runtime recovery and listener readiness remain separate actor operations.

use std::{fmt, sync::Arc};

mod production;
#[cfg(feature = "test-support")]
pub mod test_support;
pub use production::{RecoveredAccountServer, production_run, production_stopped_owner};

use spaceterm_core::{
    identity::ServerId,
    operations::RequestDigest,
    organization::ServerDirectory,
    pairing::{EnrollmentProposal, PairingFailure},
    storage::StoreFailure,
};
use spaceterm_host::{
    account_service::{RegisteredServer, ServiceFailure, ServiceSupervisor},
    organization_store::{SqliteOrganizationStore, StoreIdentity, StoreLimits, migrations, schema},
    server_keys::{KeyStoreFailure, ServerIdentity, ServerKeyStore},
    startup::DesiredServerState,
    storage::{Database, StorageFailure},
};

use crate::{contracts::ServerCredentialPort, pairing::management::AccountOwnerEnrollmentPort};
use spaceterm_host::native_deployment::contracts::NativeRequestId;
use spaceterm_protocol::pairing_adapter::enrollment_proposal_digest;

/// Account-owner enrollment binds deliberate issuance to the retained host journal.
/// Both pairing callbacks only validate persisted authority, including after restart.
/// Neither validation nor issuance starts a deliberately stopped server.
pub struct AccountOwnerEnrollment {
    supervisor: Arc<ServiceSupervisor>,
}
impl AccountOwnerEnrollment {
    pub fn new(supervisor: Arc<ServiceSupervisor>) -> Self {
        Self { supervisor }
    }

    /// Called only by deliberate owner issuance, after the host reserved the request.
    /// The proposal is public comparison data; pairing separately verifies its proof.
    pub fn record_authorization(
        &self,
        request: NativeRequestId,
        proposal: &EnrollmentProposal,
    ) -> Result<RequestDigest, PairingFailure> {
        let server = proposal.parts().server;
        let claim =
            enrollment_proposal_digest(proposal).map_err(|_| PairingFailure::InvalidRequest)?;
        if let Some(digest) = self
            .supervisor
            .retained_owner_enrollment_digest(request, server, claim)
            .map_err(owner_failure)?
        {
            return Ok(digest);
        }
        let digest = self
            .supervisor
            .owner_enrollment_digest(request, server, claim)
            .map_err(owner_failure)?;
        self.supervisor
            .record_owner_enrollment(request, digest, server, claim)
            .map_err(owner_failure)?;
        Ok(digest)
    }
}
impl AccountOwnerEnrollmentPort for AccountOwnerEnrollment {
    fn revalidate_owner_issuance(
        &self,
        request: &NativeRequestId,
        digest: RequestDigest,
        server: ServerId,
        claim_digest: RequestDigest,
    ) -> Result<(), PairingFailure> {
        self.supervisor
            .validate_owner_enrollment(request, digest, server, claim_digest)
            .map_err(owner_failure)
    }

    fn validate_owner_request(
        &self,
        request: &NativeRequestId,
        digest: RequestDigest,
        server: ServerId,
        proposal: &EnrollmentProposal,
    ) -> Result<(), PairingFailure> {
        if proposal.parts().server != server {
            return Err(PairingFailure::IdentityMismatch);
        }
        let claim =
            enrollment_proposal_digest(proposal).map_err(|_| PairingFailure::InvalidRequest)?;
        self.revalidate_owner_issuance(request, digest, server, claim)
    }
}
pub(crate) fn owner_failure(failure: ServiceFailure) -> PairingFailure {
    match failure {
        ServiceFailure::Missing
        | ServiceFailure::UnknownRequest
        | ServiceFailure::RequestRetired => PairingFailure::AuthorizationRequired,
        ServiceFailure::IdentityMismatch => PairingFailure::IdentityMismatch,
        ServiceFailure::RequestConflict => PairingFailure::RequestIdentityReused,
        ServiceFailure::UnsafeStorage => PairingFailure::UnsafeStorage,
        ServiceFailure::UnsupportedSchema => PairingFailure::UnknownSchema,
        ServiceFailure::InvalidRecord => PairingFailure::InvalidRequest,
        ServiceFailure::LimitExceeded => PairingFailure::LimitExceeded,
        ServiceFailure::StorageLocked | ServiceFailure::StorageUnavailable => {
            PairingFailure::StorageUnavailable
        }
        ServiceFailure::Indeterminate => PairingFailure::RecoveryRequired,
        _ => PairingFailure::AuthorizationUnavailable,
    }
}

/// Startup errors contain only provider classifications, not native messages.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StartupFailure {
    Account(ServiceFailure),
    RegistrationMissing,
    DeliberatelyStopped,
    Key(KeyStoreFailure),
    Storage(StorageFailure),
    Store(StoreFailure),
    Runtime(crate::contracts::ServerFailure),
    Distribution(spaceterm_distribution::DistributionFailure),
}
impl StartupFailure {
    pub const fn classification(self) -> &'static str {
        match self {
            Self::Account(value) => match value {
                ServiceFailure::RootAccount => "server account unavailable: RootAccount",
                ServiceFailure::UnsupportedHost => "server account unavailable: UnsupportedHost",
                ServiceFailure::Missing => "server account unavailable: Missing",
                ServiceFailure::StorageLocked => "server account unavailable: StorageLocked",
                ServiceFailure::StorageUnavailable => {
                    "server account unavailable: StorageUnavailable"
                }
                ServiceFailure::UnsafeStorage => "server account unavailable: UnsafeStorage",
                ServiceFailure::InvalidRecord => "server account unavailable: InvalidRecord",
                ServiceFailure::UnsupportedSchema => {
                    "server account unavailable: UnsupportedSchema"
                }
                ServiceFailure::IdentityMismatch => "server account unavailable: IdentityMismatch",
                ServiceFailure::ArtifactRejected => "server account unavailable: ArtifactRejected",
                ServiceFailure::IncompatibleStorage => {
                    "server account unavailable: IncompatibleStorage"
                }
                ServiceFailure::UnknownRequest => "server account unavailable: UnknownRequest",
                ServiceFailure::RequestConflict => "server account unavailable: RequestConflict",
                ServiceFailure::RequestRetired => "server account unavailable: RequestRetired",
                ServiceFailure::StaleAssessment => "server account unavailable: StaleAssessment",
                ServiceFailure::ConfirmationRequired => {
                    "server account unavailable: ConfirmationRequired"
                }
                ServiceFailure::AuthorizationDenied => {
                    "server account unavailable: AuthorizationDenied"
                }
                ServiceFailure::AuthorizationCancelled => {
                    "server account unavailable: AuthorizationCancelled"
                }
                ServiceFailure::AuthorizationUnavailable => {
                    "server account unavailable: AuthorizationUnavailable"
                }
                ServiceFailure::SupervisorUnavailable => {
                    "server account unavailable: SupervisorUnavailable"
                }
                ServiceFailure::LoginSessionUnavailable => {
                    "server account unavailable: LoginSessionUnavailable"
                }
                ServiceFailure::Suppressed => "server account unavailable: Suppressed",
                ServiceFailure::LimitExceeded => "server account unavailable: LimitExceeded",
                ServiceFailure::Pending => "server account unavailable: Pending",
                ServiceFailure::CleanupUnresolved => {
                    "server account unavailable: CleanupUnresolved"
                }
                ServiceFailure::Indeterminate => "server account unavailable: Indeterminate",
            },
            Self::Key(value) => match value {
                KeyStoreFailure::Missing => "server key unavailable: Missing",
                KeyStoreFailure::StorageLocked => "server key unavailable: StorageLocked",
                KeyStoreFailure::StorageUnavailable => "server key unavailable: StorageUnavailable",
                KeyStoreFailure::UnsafeStorage => "server key unavailable: UnsafeStorage",
                KeyStoreFailure::InvalidRecord => "server key unavailable: InvalidRecord",
                KeyStoreFailure::IdentityMismatch => "server key unavailable: IdentityMismatch",
                KeyStoreFailure::AlreadyExists => "server key unavailable: AlreadyExists",
                KeyStoreFailure::Conflict => "server key unavailable: Conflict",
                KeyStoreFailure::LimitExceeded => "server key unavailable: LimitExceeded",
                KeyStoreFailure::Indeterminate => "server key unavailable: Indeterminate",
            },
            Self::Storage(value) => match value {
                StorageFailure::Unavailable => "server storage unavailable: Unavailable",
                StorageFailure::UnsafeStorage => "server storage unavailable: UnsafeStorage",
                StorageFailure::UnknownSchema { .. } => "server storage unavailable: UnknownSchema",
                StorageFailure::InvalidState => "server storage unavailable: InvalidState",
                StorageFailure::Io => "server storage unavailable: Io",
                StorageFailure::RecoveryRequired => "server storage unavailable: RecoveryRequired",
            },
            Self::Store(value) => match value {
                StoreFailure::Locked => "server storage unavailable: Locked",
                StoreFailure::Unavailable => "server storage unavailable: Unavailable",
                StoreFailure::UnsafeStorage => "server storage unavailable: UnsafeStorage",
                StoreFailure::UnknownSchema => "server storage unavailable: UnknownSchema",
                StoreFailure::InvalidDurableState => {
                    "server storage unavailable: InvalidDurableState"
                }
                StoreFailure::Io => "server storage unavailable: Io",
                StoreFailure::RevisionConflict => "server storage unavailable: RevisionConflict",
                StoreFailure::Exhausted => "server storage unavailable: Exhausted",
                StoreFailure::InvalidTransaction => {
                    "server storage unavailable: InvalidTransaction"
                }
                StoreFailure::IdentityMismatch => "server storage unavailable: IdentityMismatch",
                StoreFailure::RecoveryRequired => "server storage unavailable: RecoveryRequired",
            },
            Self::Runtime(value) => match value.class {
                crate::contracts::ServerFailureClass::Authentication => {
                    "server runtime unavailable: Authentication"
                }
                crate::contracts::ServerFailureClass::ScopeMismatch => {
                    "server runtime unavailable: ScopeMismatch"
                }
                crate::contracts::ServerFailureClass::StaleAuthority => {
                    "server runtime unavailable: StaleAuthority"
                }
                crate::contracts::ServerFailureClass::Capacity => {
                    "server runtime unavailable: Capacity"
                }
                crate::contracts::ServerFailureClass::Exhausted => {
                    "server runtime unavailable: Exhausted"
                }
                crate::contracts::ServerFailureClass::StorageUnavailable => {
                    "server runtime unavailable: StorageUnavailable"
                }
                crate::contracts::ServerFailureClass::RecoveryRequired => {
                    "server runtime unavailable: RecoveryRequired"
                }
                crate::contracts::ServerFailureClass::RuntimeUnavailable => {
                    "server runtime unavailable: RuntimeUnavailable"
                }
                crate::contracts::ServerFailureClass::NativeUnavailable => {
                    "server runtime unavailable: NativeUnavailable"
                }
                crate::contracts::ServerFailureClass::CleanupIncomplete => {
                    "server runtime unavailable: CleanupIncomplete"
                }
            },
            Self::Distribution(value) => match value {
                spaceterm_distribution::DistributionFailure::Verification => {
                    "server distribution metadata unavailable: Verification"
                }
                spaceterm_distribution::DistributionFailure::InvalidRecord => {
                    "server distribution metadata unavailable: InvalidRecord"
                }
                spaceterm_distribution::DistributionFailure::UnsupportedSchema => {
                    "server distribution metadata unavailable: UnsupportedSchema"
                }
                spaceterm_distribution::DistributionFailure::IdentityMismatch => {
                    "server distribution metadata unavailable: IdentityMismatch"
                }
                spaceterm_distribution::DistributionFailure::LimitExceeded => {
                    "server distribution metadata unavailable: LimitExceeded"
                }
                spaceterm_distribution::DistributionFailure::ArchiveRejected => {
                    "server distribution metadata unavailable: ArchiveRejected"
                }
                spaceterm_distribution::DistributionFailure::ManifestRejected => {
                    "server distribution metadata unavailable: ManifestRejected"
                }
            },
            Self::RegistrationMissing => "server registration missing",
            Self::DeliberatelyStopped => "server deliberately stopped",
        }
    }
}

impl ServerCredentialPort for ServerIdentity {}

/// Observes only the registered existing database. This boundary never receives
/// initialization authority and must not create or replace storage.
pub trait StartupStorageObserver: Send + Sync {
    fn open_existing(&self, registration: &RegisteredServer) -> Result<Database, StorageFailure>;
}

pub struct NativeStartupStorage;
impl StartupStorageObserver for NativeStartupStorage {
    fn open_existing(&self, registration: &RegisteredServer) -> Result<Database, StorageFailure> {
        Database::open_existing_for_application(
            registration.organization_directory(),
            migrations(),
            schema::APPLICATION_ID,
        )
    }
}

/// Owns the established key and the one live SQLite authority. No endpoint has
/// been bound and no runtime or protocol principal has been created at this stage.
pub struct AccountStartup {
    registration: RegisteredServer,
    credentials: Arc<ServerIdentity>,
    database: Database,
    admission: Option<Arc<spaceterm_host::account_service::AccountStartup>>,
}
impl fmt::Debug for AccountStartup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AccountStartup([redacted])")
    }
}
impl AccountStartup {
    /// Uses only a host-validated registration. Stopped is checked before key or
    /// organization reads. Missing and unavailable files grant no creation authority.
    pub fn open(registration: RegisteredServer) -> Result<Self, StartupFailure> {
        if registration.desired_state() != DesiredServerState::Running {
            return Err(StartupFailure::DeliberatelyStopped);
        }
        let keys = ServerKeyStore::open(&registration).map_err(StartupFailure::Key)?;
        let credentials = keys
            .load_identity(registration.server_pin())
            .map_err(StartupFailure::Key)?;
        let database = Database::open_existing_for_application(
            registration.organization_directory(),
            migrations(),
            schema::APPLICATION_ID,
        )
        .map_err(StartupFailure::Storage)?;
        Ok(Self {
            registration,
            credentials,
            database,
            admission: None,
        })
    }

    /// Retains the host's instance lease and the already loaded identity. The
    /// organization database is opened once, after native admission succeeds.
    pub fn admit(supervisor: &ServiceSupervisor) -> Result<Self, StartupFailure> {
        Self::admit_with_storage(supervisor, &NativeStartupStorage)
    }

    /// Waits in this process for retained storage. The delay grows to 30 seconds,
    /// not a retry-count limit that would exhaust native restart supervision.
    /// Every attempt checks durable Stop again and preserves the first observed
    /// server identity and organization root. No attempt initializes storage.
    pub async fn admit_with_storage_wait(
        supervisor: &ServiceSupervisor,
        storage: Arc<dyn StartupStorageObserver>,
    ) -> Result<Self, StartupFailure> {
        let mut delay = std::time::Duration::from_millis(250);
        let mut expected = None;
        loop {
            let observed = supervisor.discover().map_err(StartupFailure::Account);
            let result = match observed {
                Ok(Some(registration)) => {
                    if registration.desired_state() == DesiredServerState::Stopped {
                        return Err(StartupFailure::DeliberatelyStopped);
                    }
                    let identity = (
                        registration.server_pin(),
                        registration.organization_directory().to_owned(),
                    );
                    if expected.as_ref().is_some_and(|old| old != &identity) {
                        return Err(StartupFailure::Account(ServiceFailure::IdentityMismatch));
                    }
                    expected = Some(identity);
                    Self::admit_with_storage(supervisor, storage.as_ref())
                }
                Ok(None) => return Err(StartupFailure::RegistrationMissing),
                // Only explicit startup admission may reconcile an interrupted signed
                // custody publication. Read-only discovery must never repair it.
                Err(StartupFailure::Account(ServiceFailure::Indeterminate)) => {
                    Self::admit_with_storage(supervisor, storage.as_ref())
                }
                Err(error) => Err(error),
            };
            match result {
                Ok(startup) => {
                    if expected.as_ref().is_some_and(|(pin, organization)| {
                        *pin != startup.registration().server_pin()
                            || organization != startup.registration().organization_directory()
                    }) {
                        return Err(StartupFailure::Account(ServiceFailure::IdentityMismatch));
                    }
                    return Ok(startup);
                }
                Err(
                    StartupFailure::Account(
                        ServiceFailure::StorageLocked | ServiceFailure::StorageUnavailable,
                    )
                    | StartupFailure::Key(
                        KeyStoreFailure::StorageLocked | KeyStoreFailure::StorageUnavailable,
                    )
                    | StartupFailure::Storage(StorageFailure::Unavailable),
                ) => {
                    tokio::time::sleep(delay).await;
                    delay = delay
                        .saturating_mul(2)
                        .min(std::time::Duration::from_secs(30));
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn admit_with_storage(
        supervisor: &ServiceSupervisor,
        storage: &dyn StartupStorageObserver,
    ) -> Result<Self, StartupFailure> {
        use spaceterm_host::account_service::AccountStartupAdmission;
        let admission = match supervisor.admit_startup().map_err(|failure| {
            if failure == ServiceFailure::Missing {
                StartupFailure::RegistrationMissing
            } else {
                StartupFailure::Account(failure)
            }
        })? {
            AccountStartupAdmission::Stopped => return Err(StartupFailure::DeliberatelyStopped),
            AccountStartupAdmission::WaitingStorage(wait) => {
                return Err(StartupFailure::Account(wait.failure()));
            }
            AccountStartupAdmission::Ready(admission) => Arc::new(admission),
        };
        let registration = supervisor
            .discover()
            .map_err(StartupFailure::Account)?
            .ok_or(StartupFailure::RegistrationMissing)?;
        admission.revalidate().map_err(StartupFailure::Account)?;
        if registration.server_pin() != admission.registration().server_pin() {
            return Err(StartupFailure::Account(ServiceFailure::IdentityMismatch));
        }
        let database = storage
            .open_existing(&registration)
            .map_err(StartupFailure::Storage)?;
        Ok(Self {
            credentials: admission.identity(),
            registration,
            database,
            admission: Some(admission),
        })
    }

    pub fn registration(&self) -> &RegisteredServer {
        &self.registration
    }

    pub fn credentials(&self) -> Arc<dyn ServerCredentialPort> {
        self.credentials.clone()
    }

    /// Transfers the live database to its real domain projection without reopening
    /// the inode. The composing account provider supplies the owning account home;
    /// a protocol path or caller environment is not a source for that fact.
    pub fn open_store(self, home: ServerDirectory) -> Result<AccountStore, StartupFailure> {
        let identity = StoreIdentity {
            server: ServerId::new(self.registration.server_pin().identity),
            account: self.registration.key(),
            server_public_key: self.credentials.pin().public_key,
        };
        let services = crate::adapters::store::production_services(identity, home)
            .map_err(StartupFailure::Store)?;
        let store = SqliteOrganizationStore::open_existing(
            self.database,
            identity,
            StoreLimits::production(),
            services,
        )
        .map_err(StartupFailure::Store)?;
        Ok(AccountStore {
            registration: self.registration,
            credentials: self.credentials,
            store,
            admission: self.admission,
        })
    }
}

/// Validated real providers to transfer into the actor's existing dependencies.
pub struct AccountStore {
    pub registration: RegisteredServer,
    pub credentials: Arc<ServerIdentity>,
    pub store: SqliteOrganizationStore,
    admission: Option<Arc<spaceterm_host::account_service::AccountStartup>>,
}
impl fmt::Debug for AccountStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AccountStore([redacted])")
    }
}

/// Resolves the current nonroot account and compiled build before reading its
/// fixed registration. Runtime flags and caller XDG roots cannot select it.
pub fn production_startup() -> Result<AccountStartup, StartupFailure> {
    let supervisor = ServiceSupervisor::current(None).map_err(StartupFailure::Account)?;
    AccountStartup::admit(&supervisor)
}

/// Read-only registration facts. Running is durable intent, not a claim that a
/// process or listener is live. Native supervision reports those facts separately.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(tag = "registration", rename_all = "lowercase")]
pub enum AccountStatus {
    Missing {
        build_identity: spaceterm_host::startup::BuildIdentity,
    },
    Registered {
        build_identity: spaceterm_host::startup::BuildIdentity,
        desired_state: DesiredServerState,
    },
}

/// Queries only retained account registration. Missing registration is an ordinary
/// status result, not permission to initialize keys, storage or a service.
pub fn account_status(supervisor: &ServiceSupervisor) -> Result<AccountStatus, StartupFailure> {
    let build_identity = supervisor.key().build_identity;
    Ok(
        match supervisor.discover().map_err(StartupFailure::Account)? {
            Some(registration) => AccountStatus::Registered {
                build_identity,
                desired_state: registration.desired_state(),
            },
            None => AccountStatus::Missing { build_identity },
        },
    )
}

/// The account/build identity is selected by native discovery, never caller roots.
pub fn production_status() -> Result<Vec<u8>, StartupFailure> {
    let supervisor = ServiceSupervisor::current(None).map_err(StartupFailure::Account)?;
    let result = account_status(&supervisor)?;
    serde_json::to_vec(&serde_json::json!({"schema": 1, "result": result}))
        .map_err(|_| StartupFailure::Account(ServiceFailure::InvalidRecord))
}
