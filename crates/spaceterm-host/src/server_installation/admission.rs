use super::{RegisteredServer, ServiceFailure};
use crate::startup::{AccountBuildKey, InstallationId};

/// Installation effects that the account lifecycle owner must admit for its original request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstallationOperation {
    /// Open account code storage. This does not admit a code or selection change.
    Open,
    /// Publish only this authenticated archive as staging.
    Stage {
        /// Exact signed archive digest from the original request.
        artifact_sha256: [u8; 32],
    },
    /// Select this installation after confirmed, completed runtime retirement.
    Activate {
        /// Exact staged installation, never the newest observed directory.
        installation: InstallationId,
        /// Expected installation selection generation.
        generation: u64,
        /// Existing organization storage schema.
        storage_schema: u32,
    },
    /// Select the retained previous version after confirmed runtime retirement.
    Rollback {
        /// Expected installation selection generation.
        generation: u64,
        /// Existing organization storage schema.
        storage_schema: u32,
    },
    /// Abandon only this original staged or preparing installation.
    Discard {
        /// Original candidate identity.
        installation: InstallationId,
        /// Expected installation selection generation.
        generation: u64,
    },
    /// Finish only cleanup already recorded by this installation store.
    Cleanup,
}

/// Injected lifecycle admission until the account-service provider supplies its capability.
///
/// The provider retains its registration mutation lock and original durable request
/// for the whole scope. Verification binds the exact registration generation and
/// operation, rejects revoked or uncertain admission, and requires completed
/// quiescence for Activate and Rollback. It must not re-admit a historical request.
/// No production implementation or permissive default exists in this module.
pub trait InstallationAdmission {
    /// Revalidates the original account-service admission immediately before an effect.
    fn verify(
        &self,
        registered: &RegisteredServer,
        operation: InstallationOperation,
    ) -> Result<(), ServiceFailure>;
}

/// Borrowed account-service admission and retained native registration authority.
/// This scope cannot outlive the provider's registration lock or durable request.
pub struct InstallationScope<'a> {
    pub(super) registered: &'a RegisteredServer,
    admission: &'a dyn InstallationAdmission,
}
impl<'a> InstallationScope<'a> {
    /// Admits code storage without manufacturing lifecycle authority from registration facts.
    pub fn new(
        registered: &'a RegisteredServer,
        admission: &'a dyn InstallationAdmission,
    ) -> Result<Self, ServiceFailure> {
        let scope = Self {
            registered,
            admission,
        };
        scope.verify(registered.key(), InstallationOperation::Open)?;
        Ok(scope)
    }
    pub(super) fn verify(
        &self,
        key: AccountBuildKey,
        operation: InstallationOperation,
    ) -> Result<(), ServiceFailure> {
        if self.registered.key() != key {
            return Err(ServiceFailure::IdentityMismatch);
        }
        self.registered.directory.verify()?;
        self.admission.verify(self.registered, operation)
    }
}
