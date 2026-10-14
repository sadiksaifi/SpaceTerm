use super::{
    InstallationStatus, RegisteredServer, ServiceFailure, ServicePhase, ServiceProgress,
    ServiceSupervisor,
};
use crate::{
    organization_store::schema::SCHEMA_VERSION,
    private_filesystem::Lease,
    server_installation::{
        InstallationAdmission, InstallationOperation, InstallationScope, ServerInstallationStore,
    },
};

impl ServiceSupervisor {
    fn activation_store(
        &self,
        registration: &RegisteredServer,
    ) -> Result<ServerInstallationStore, ServiceFailure> {
        #[cfg(feature = "test-support")]
        if let Some(key) = &self.test_installation_trust {
            return crate::server_installation::test_support::existing_store(
                registration,
                key.clone(),
            );
        }
        ServerInstallationStore::open_existing(registration)
    }

    pub(super) fn verify_staged_activation(
        &self,
        registration: &RegisteredServer,
        target: &InstallationStatus,
    ) -> Result<(), ServiceFailure> {
        let store = self.activation_store(registration)?;
        let actual = store
            .retain_compatible_staged(target.installation, SCHEMA_VERSION)?
            .status();
        if actual.installation != target.installation
            || actual.version != target.version
            || actual.artifact_sha256 != target.artifact_sha256
        {
            return Err(ServiceFailure::IdentityMismatch);
        }
        Ok(())
    }

    pub(super) fn activate_installation_selection(
        &self,
        progress: &ServiceProgress,
        registration: &RegisteredServer,
        lease: &Lease,
        target: &InstallationStatus,
        previous: Option<&InstallationStatus>,
    ) -> Result<(), ServiceFailure> {
        lease.verify()?;
        if progress.phase != ServicePhase::Publishing {
            return Err(ServiceFailure::RequestConflict);
        }
        let store = self.activation_store(registration)?;
        let selection = store.snapshot()?;
        if selection.approved == Some(target.installation) {
            // Selection publication may precede registration publication. Only
            // the original target with the original predecessor can converge.
            if selection.staged.is_some()
                || selection.previous != previous.map(|status| status.installation)
            {
                return Err(ServiceFailure::RequestConflict);
            }
        } else {
            if selection.approved != previous.map(|status| status.installation)
                || selection.staged != Some(target.installation)
            {
                return Err(ServiceFailure::RequestConflict);
            }
            self.verify_staged_activation(registration, target)?;
            let admission = ActivationAdmission {
                supervisor: self,
                progress,
                lease,
                target,
                generation: selection.generation,
            };
            let scope = InstallationScope::new(registration, &admission)?;
            store.activate(
                &scope,
                target.installation,
                selection.generation,
                SCHEMA_VERSION,
            )?;
        }
        let actual = store.retain_compatible_approved(SCHEMA_VERSION)?.status();
        if actual.installation != target.installation
            || actual.version != target.version
            || actual.artifact_sha256 != target.artifact_sha256
        {
            return Err(ServiceFailure::IdentityMismatch);
        }
        lease.verify().map_err(Into::into)
    }
}

struct ActivationAdmission<'a> {
    supervisor: &'a ServiceSupervisor,
    progress: &'a ServiceProgress,
    lease: &'a Lease,
    target: &'a InstallationStatus,
    generation: u64,
}
impl InstallationAdmission for ActivationAdmission<'_> {
    fn verify(
        &self,
        registration: &RegisteredServer,
        operation: InstallationOperation,
    ) -> Result<(), ServiceFailure> {
        self.lease.verify()?;
        let original = self.supervisor.request_progress(self.progress.request)?;
        let current = self.supervisor.discover()?.ok_or(ServiceFailure::Missing)?;
        if original != *self.progress
            || original.phase != ServicePhase::Publishing
            || current.generation() != registration.generation()
            || current.server_pin() != registration.server_pin()
            || current.staged_installation() != Some(self.target)
        {
            return Err(ServiceFailure::RequestConflict);
        }
        match operation {
            InstallationOperation::Open => Ok(()),
            InstallationOperation::Activate {
                installation,
                generation,
                storage_schema,
            } if installation == self.target.installation
                && generation == self.generation
                && storage_schema == SCHEMA_VERSION =>
            {
                Ok(())
            }
            _ => Err(ServiceFailure::RequestConflict),
        }
    }
}
