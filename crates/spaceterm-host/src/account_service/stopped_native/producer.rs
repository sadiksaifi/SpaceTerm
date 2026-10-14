//! Expected-native scope checks for closed stopped account producer operations.

use spaceterm_distribution::BuildVersion;
use spaceterm_protocol::authentication::ServerPin;

use super::super::{
    ServiceFailure, ServiceSupervisor,
    install::InstallRecord,
    registration::{REGISTRATION_LIMIT, Registration},
    supervisor::{JOURNAL_LIMIT, Journal, decode},
};
use super::{Execution, StoppedNativeAction, StoppedNativeRequest};
use crate::{
    native_deployment::contracts::NativeRequestId, private_filesystem::Directory,
    startup::AccountBuildKey,
};

impl ServiceSupervisor {
    /// Reconciles only this existing account, retained pin and original identifier.
    /// It validates private native records before clearing publication uncertainty;
    /// no decoded principal, assessment or native effect is admitted here.
    pub fn reconcile_stopped_native_storage(
        &self,
        expected_key: AccountBuildKey,
        expected_pin: ServerPin,
        request: NativeRequestId,
    ) -> Result<(), ServiceFailure> {
        if self.key != expected_key {
            return Err(ServiceFailure::IdentityMismatch);
        }
        let directory =
            Directory::open_for_recovery(&self.anchor)?.ok_or(ServiceFailure::Missing)?;
        let registration = directory
            .read_for_recovery("registration", REGISTRATION_LIMIT)?
            .ok_or(ServiceFailure::Missing)?;
        let record: Registration = decode(&registration.bytes, REGISTRATION_LIMIT)?;
        record.validate(self.key)?;
        if record.server != expected_pin.identity || record.public_key != expected_pin.public_key {
            return Err(ServiceFailure::IdentityMismatch);
        }
        let requests = directory
            .read_for_recovery("requests", JOURNAL_LIMIT)?
            .ok_or(ServiceFailure::UnknownRequest)?;
        let journal: Journal = decode(&requests.bytes, JOURNAL_LIMIT)?;
        journal.validate(self.key)?;
        let slot = &journal.reservations[journal.reservation(request)?];
        if slot.owner.is_some()
            || slot.runtime_intent.is_some()
            || slot.running_stage.is_some()
            || (slot.stopped_native.is_none() && (slot.service.is_some() || slot.install.is_some()))
        {
            return Err(ServiceFailure::RequestConflict);
        }
        if let Some(stopped) = &slot.stopped_native
            && (stopped.intent.basis.server != expected_pin.identity
                || stopped.intent.basis.public_key != expected_pin.public_key)
        {
            return Err(ServiceFailure::IdentityMismatch);
        }
        self.reconcile_storage(request)?;
        let current = self.discover()?.ok_or(ServiceFailure::Missing)?;
        if current.key() != expected_key || current.server_pin() != expected_pin {
            return Err(ServiceFailure::IdentityMismatch);
        }
        Ok(())
    }

    /// Checks only immutable exact Stage comparisons before bounded transfer.
    /// Accepted-only re-supply requires the retained server-signed paired origin;
    /// it never updates a proof, issue time, expiry, action or request identifier.
    pub fn check_stopped_native_stage_transfer(
        &self,
        original: StoppedNativeRequest,
        artifact_sha256: [u8; 32],
        version: &BuildVersion,
        accepted_only: bool,
    ) -> Result<(), ServiceFailure> {
        let record = self.stopped_record(original)?;
        if accepted_only && (!record.accepted || record.origin.is_none()) {
            return Err(ServiceFailure::Pending);
        }
        if !matches!(record.intent.action, StoppedNativeAction::Stage { artifact_sha256: expected } if expected == artifact_sha256)
        {
            return Err(ServiceFailure::RequestConflict);
        }
        if !record.accepted
            && record.intent.basis.staged.as_ref().is_some_and(|staged| {
                staged.artifact_sha256 != artifact_sha256 || staged.version != *version
            })
        {
            return Err(ServiceFailure::RequestConflict);
        }
        if let Some(execution) = &record.execution {
            let Execution::Stage(execution) = execution else {
                return Err(ServiceFailure::RequestConflict);
            };
            let candidate = InstallRecord::stage(
                self.key,
                original.request,
                artifact_sha256,
                version.clone(),
                record.intent.basis.clone(),
            )?;
            if candidate.progress(original.request).request_digest
                != execution.progress(original.request).request_digest
            {
                return Err(ServiceFailure::RequestConflict);
            }
        } else if accepted_only {
            return Err(ServiceFailure::Pending);
        }
        Ok(())
    }
}
