use std::path::PathBuf;

use super::{RegisteredServer, ServiceFailure, ServiceInstallations};
use crate::{
    server_installation::{
        InstallationAdmission, InstallationOperation, InstallationScope, ServerInstallationStore,
    },
    startup::DesiredServerState,
};

/// Production signed account-installation selection for native service managers.
#[derive(Default)]
pub struct NativeServiceInstallations;
impl ServiceInstallations for NativeServiceInstallations {
    fn approved_executable(
        &self,
        registration: &RegisteredServer,
    ) -> Result<PathBuf, ServiceFailure> {
        let expected = registration
            .approved_installation()
            .ok_or(ServiceFailure::Missing)?;
        let store = ServerInstallationStore::open_existing(registration)?;
        let executable =
            store.retain_compatible_approved(crate::organization_store::schema::SCHEMA_VERSION)?;
        let actual = executable.status();
        if actual.installation != expected.installation
            || actual.version != expected.version
            || actual.artifact_sha256 != expected.artifact_sha256
        {
            return Err(ServiceFailure::IdentityMismatch);
        }
        Ok(executable.path().to_owned())
    }
    fn uninstall(&self, registration: &RegisteredServer) -> Result<(), ServiceFailure> {
        if registration.desired_state() != DesiredServerState::Stopped {
            return Err(ServiceFailure::ConfirmationRequired);
        }
        let store = match ServerInstallationStore::open_existing(registration) {
            Ok(store) => store,
            Err(ServiceFailure::Missing) if !registration.is_installed() => return Ok(()),
            Err(error) => return Err(error),
        };
        let scope = InstallationScope::new(registration, &StoppedRemoval)?;
        store.uninstall(&scope)
    }
}
struct StoppedRemoval;
impl InstallationAdmission for StoppedRemoval {
    fn verify(
        &self,
        registered: &RegisteredServer,
        operation: InstallationOperation,
    ) -> Result<(), ServiceFailure> {
        if registered.desired_state() != DesiredServerState::Stopped {
            return Err(ServiceFailure::ConfirmationRequired);
        }
        if !matches!(
            operation,
            InstallationOperation::Open | InstallationOperation::Uninstall
        ) {
            return Err(ServiceFailure::RequestConflict);
        }
        registered.directory.verify().map_err(Into::into)
    }
}
