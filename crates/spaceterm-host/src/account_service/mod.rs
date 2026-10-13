//! Account-owned service discovery and durable request reservations.
//!
//! This module does not yet supply lifecycle mutation admission. Requests are
//! reserved before an explicit caller submits work. Read-only owner validation
//! never initializes storage or issues replacement authority.

mod owner_enrollment;
mod registration;
mod supervisor;

pub use registration::RegisteredServer;
pub use supervisor::ServiceSupervisor;

/// Content-free account service failures. No utility output or paths are retained.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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

    use std::{
        path::{Path, PathBuf},
        sync::atomic::AtomicBool,
    };

    use super::{
        RegisteredServer, ServiceFailure, ServiceSupervisor,
        registration::{REGISTRATION_LIMIT, Registration},
        supervisor::encode,
    };
    use crate::{
        private_filesystem::Directory,
        server_keys::{IdentityCreatePermit, KeyStoreFailure, ServerKeyStore},
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
                indeterminate: AtomicBool::new(false),
            }
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
            let supervisor = self.reopen();
            if let Some(registration) = supervisor.discover()? {
                return Ok(registration);
            }
            let anchor = Directory::create(&self.anchor)?;
            Directory::create(&self.runtime)?;
            let (store, permit) = self.identity_scope()?;
            let identity = match store.create_identity(permit) {
                Ok(identity) => identity,
                Err(KeyStoreFailure::AlreadyExists) => {
                    store.load_initial_identity().map_err(classify_keys)?
                }
                Err(error) => return Err(classify_keys(error)),
            };
            let pin = identity.pin();
            let record = Registration {
                schema: 1,
                key: self.key,
                generation: 1,
                desired,
                server: pin.identity,
                public_key: pin.public_key,
                data: self.data.clone(),
                organization: self.data.join("organization"),
                runtime: self.runtime.clone(),
            };
            record.validate(self.key)?;
            let bytes = encode(&record, REGISTRATION_LIMIT)?;
            match anchor.publish("registration", &bytes, None) {
                Ok(()) | Err(crate::private_filesystem::Failure::Conflict) => {
                    supervisor.discover()?.ok_or(ServiceFailure::Indeterminate)
                }
                Err(error) => Err(error.into()),
            }
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
    /// Supplies the native committed-but-unsynced outcome for the next publication.
    /// This is a commit-result fault, not a power-loss or filesystem fault simulation.
    pub fn fail_next_publication_durability() {
        crate::private_filesystem::test_support::fail_next_publication_durability();
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

    fn classify_keys(error: KeyStoreFailure) -> ServiceFailure {
        match error {
            KeyStoreFailure::Missing => ServiceFailure::Missing,
            KeyStoreFailure::StorageLocked => ServiceFailure::StorageLocked,
            KeyStoreFailure::StorageUnavailable => ServiceFailure::StorageUnavailable,
            KeyStoreFailure::UnsafeStorage => ServiceFailure::UnsafeStorage,
            KeyStoreFailure::InvalidRecord => ServiceFailure::InvalidRecord,
            KeyStoreFailure::IdentityMismatch => ServiceFailure::IdentityMismatch,
            KeyStoreFailure::AlreadyExists | KeyStoreFailure::Conflict => {
                ServiceFailure::RequestConflict
            }
            KeyStoreFailure::LimitExceeded => ServiceFailure::LimitExceeded,
            KeyStoreFailure::Indeterminate => ServiceFailure::Indeterminate,
        }
    }
}
