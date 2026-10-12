//! Operation-bound owner-store results. Root never opens or parses application data.

use super::{NativeHeadStorage, NativeRecordSync};
use crate::{
    activation_journal::{
        ActivationJournalRecord, ActivationJournalState as Phase, NativeOperationKind,
    },
    installation::{MigrationProgress, ServerDataCompatibilityPort, ServerMigrationPort},
    native_deployment::contracts::*,
    native_registration::{encode_registration_head, writer::RegistrationPublicationStorage},
};

impl<S: NativeRecordSync> NativeHeadStorage<S> {
    /// Consumes a result independently revalidated by the owning-account migration port.
    /// Install records its initial data while Staged; Running Activate records MigrationReady.
    /// This operation never repeats prepare_generation after an interruption.
    pub async fn record_migrated_data(
        &self,
        data: DataGenerationId,
        migration: &dyn ServerMigrationPort,
    ) -> PublicationDisposition<()> {
        let prepare = || {
            self.lease.revalidate()?;
            let current = self.lease.records.read_journal(self.lease.locks.key)?;
            if !matches!(
                (current.operation, current.state),
                (NativeOperationKind::Install, Phase::Staged)
                    | (NativeOperationKind::Activate, Phase::Migrating)
            ) || current.desired_state != DesiredServerState::Running
                && current.operation == NativeOperationKind::Activate
                || current.candidate_data_generation.is_some()
            {
                return Err(NativeFailure::UnsafeNativeState);
            }
            let snapshot = self.lease.records.read_snapshot(current.key)?;
            if data == current.original_data_generation
                || data.get() <= snapshot.head.data_high_water
            {
                return Err(NativeFailure::GenerationMismatch);
            }
            Ok(current)
        };
        let current = match prepare() {
            Ok(current) => current,
            Err(error) => return PublicationDisposition::NotApplied(error),
        };
        match migration.validate_generation(current.request, data).await {
            Ok(MigrationProgress::Ready(selected)) if selected == data => (),
            Ok(MigrationProgress::Ready(_)) => {
                return PublicationDisposition::NotApplied(NativeFailure::IdentityMismatch);
            }
            Ok(MigrationProgress::Pending) => {
                return PublicationDisposition::NotApplied(NativeFailure::OutcomePending);
            }
            Ok(MigrationProgress::NotApplied(error)) | Err(error) => {
                return PublicationDisposition::NotApplied(error);
            }
            Ok(MigrationProgress::Indeterminate) => {
                if let Ok(uncertain) = current.transition(Phase::RecoveryRequired) {
                    let _ = self.advance_journal(&uncertain);
                }
                return PublicationDisposition::Indeterminate;
            }
        }
        let mut next = current.clone();
        next.candidate_data_generation = Some(data);
        if current.operation == NativeOperationKind::Activate {
            next = match next.transition(Phase::MigrationReady) {
                Ok(next) => next,
                Err(error) => return PublicationDisposition::NotApplied(error),
            };
        }
        self.publish_data_result(&current, &next)
    }

    /// Revalidates stopped-reader compatibility and records the original data without migration.
    pub async fn record_stopped_data(
        &self,
        compatibility: &dyn ServerDataCompatibilityPort,
    ) -> PublicationDisposition<()> {
        let current = match self
            .lease
            .revalidate()
            .and_then(|()| self.lease.records.read_journal(self.lease.locks.key))
        {
            Ok(current) => current,
            Err(error) => return PublicationDisposition::NotApplied(error),
        };
        let next =
            match crate::installation::validate_stopped_activation_data(&current, compatibility)
                .await
            {
                Ok(next) => next,
                Err(error) => return PublicationDisposition::NotApplied(error),
            };
        self.publish_data_result(&current, &next)
    }

    fn publish_data_result(
        &self,
        current: &ActivationJournalRecord,
        next: &ActivationJournalRecord,
    ) -> PublicationDisposition<()> {
        let prepare = || {
            self.lease.revalidate()?;
            if self.lease.records.read_journal(current.key)? != *current {
                return Err(NativeFailure::GenerationMismatch);
            }
            let snapshot = self.lease.records.read_snapshot(current.key)?;
            next.recovery_selection(&snapshot.head, &snapshot.profile.account().sha256())?;
            Ok(())
        };
        if let Err(error) = prepare() {
            return PublicationDisposition::NotApplied(error);
        }
        // The journal binds the result before allocation publication. An interrupted result
        // remains recoverable under this request, never a reusable unrecorded allocation.
        match self
            .publish_journal(Some(current), next)
            .and_then(|()| self.recover_journal().map(|_| ()))
            .and_then(|()| self.recover_data_result())
        {
            Ok(()) => PublicationDisposition::Durable(()),
            Err(_) => PublicationDisposition::Indeterminate,
        }
    }

    /// Burns the selected journal's validated data ID after interrupted result publication.
    /// Returns validated progress only; an earlier Indeterminate disposition stays unchanged.
    pub fn recover_data_result(&self) -> Result<(), NativeFailure> {
        self.lease.revalidate()?;
        let current = self.lease.records.read_journal(self.lease.locks.key)?;
        if !matches!(
            (current.operation, current.state),
            (NativeOperationKind::Install, Phase::Staged)
                | (NativeOperationKind::Activate, Phase::MigrationReady)
        ) {
            return Err(NativeFailure::UnsafeNativeState);
        }
        let data = current
            .candidate_data_generation
            .ok_or(NativeFailure::OutcomePending)?;
        let snapshot = self.lease.records.read_snapshot(current.key)?;
        if data.get() <= snapshot.head.data_high_water {
            return Ok(());
        }
        let mut next = snapshot.head;
        next.generation = std::num::NonZeroU64::new(
            next.generation
                .get()
                .checked_add(1)
                .ok_or(NativeFailure::LimitExceeded)?,
        )
        .ok_or(NativeFailure::LimitExceeded)?;
        next.data_high_water = data.get();
        self.write_fresh(next.key, &encode_registration_head(&next)?)?;
        self.sync_fresh()?;
        self.replace_head(next.key)?;
        self.sync_parent()?;
        self.recover()?;
        Ok(())
    }
}
