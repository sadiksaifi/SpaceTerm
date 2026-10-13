//! Established account startup opens retained keys and storage, never initializes
//! them. Runtime recovery and listener readiness remain separate actor operations.

use std::{fmt, sync::Arc};

use spaceterm_core::{identity::ServerId, organization::ServerDirectory, storage::StoreFailure};
use spaceterm_host::{
    account_service::{RegisteredServer, ServiceFailure, ServiceSupervisor},
    organization_store::{SqliteOrganizationStore, StoreIdentity, StoreLimits, migrations, schema},
    server_keys::{KeyStoreFailure, ServerIdentity, ServerKeyStore},
    startup::DesiredServerState,
    storage::{Database, StorageFailure},
};

use crate::contracts::ServerCredentialPort;

/// Startup errors contain only provider classifications, not native messages.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StartupFailure {
    Account(ServiceFailure),
    RegistrationMissing,
    DeliberatelyStopped,
    Key(KeyStoreFailure),
    Storage(StorageFailure),
    Store(StoreFailure),
    RuntimeCompositionUnavailable,
}
impl StartupFailure {
    pub const fn classification(self) -> &'static str {
        match self {
            Self::Account(_) => "server account unavailable",
            Self::RegistrationMissing => "server registration missing",
            Self::DeliberatelyStopped => "server deliberately stopped",
            Self::Key(_) => "server key unavailable",
            Self::Storage(_) | Self::Store(_) => "server storage unavailable",
            Self::RuntimeCompositionUnavailable => "server runtime composition unavailable",
        }
    }
}

impl ServerCredentialPort for ServerIdentity {}

/// Owns the established key and the one live SQLite authority. No endpoint has
/// been bound and no runtime or protocol principal has been created at this stage.
pub struct AccountStartup {
    registration: RegisteredServer,
    credentials: Arc<ServerIdentity>,
    database: Database,
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
        })
    }
}

/// Validated real providers to transfer into the actor's existing dependencies.
pub struct AccountStore {
    pub registration: RegisteredServer,
    pub credentials: Arc<ServerIdentity>,
    pub store: SqliteOrganizationStore,
}
impl fmt::Debug for AccountStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AccountStore([redacted])")
    }
}

/// Resolves the current nonroot account and compiled build before reading its
/// fixed registration. Runtime flags and caller XDG roots cannot select it.
pub fn production_startup() -> Result<AccountStartup, StartupFailure> {
    let supervisor = ServiceSupervisor::current().map_err(StartupFailure::Account)?;
    let registration = supervisor
        .discover()
        .map_err(StartupFailure::Account)?
        .ok_or(StartupFailure::RegistrationMissing)?;
    AccountStartup::open(registration)
}
