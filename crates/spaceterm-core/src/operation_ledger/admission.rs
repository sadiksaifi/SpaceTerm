//! Explicit stream registration and worst-case result reservation before effects.
use super::model::*;
use crate::operation_ledger_contracts::*;
use crate::operations::{Command, OperationBinding, OperationId, OperationLookup};
use crate::pairing::PairingCommand;

/// Closed, server-derived classification. Purpose comparison grants no authority.
pub fn reservation_class(
    purpose: StreamPurpose,
    command: &Command,
) -> Result<ReservationClass, LedgerFailure> {
    match purpose {
        StreamPurpose::SelfRevocation => {
            return match command {
                Command::Pairing(PairingCommand::SelfRevoke { .. })
                | Command::RevokePairing { .. } => Ok(ReservationClass::DedicatedSelfRevocation),
                _ => Err(failure(LedgerFailureClass::PurposeMismatch)),
            };
        }
        StreamPurpose::Enrollment { claim_digest } => {
            return match command {
                Command::Pairing(PairingCommand::Enroll {
                    claim_digest: actual,
                    ..
                }) if *actual == claim_digest => Ok(ReservationClass::DedicatedEnrollment),
                _ => Err(failure(LedgerFailureClass::PurposeMismatch)),
            };
        }
        StreamPurpose::Ordinary => {}
    }
    Ok(match command {
        Command::ReleaseControl(_)
        | Command::EndAllSessions { .. }
        | Command::RevokePairing { .. }
        | Command::Pairing(
            PairingCommand::UnpairConfirmed { .. } | PairingCommand::SelfRevoke { .. },
        ) => ReservationClass::Safety,
        Command::Pairing(PairingCommand::Enroll { .. }) => {
            return Err(failure(LedgerFailureClass::PurposeMismatch));
        }
        Command::CreateWorkspace { .. }
        | Command::CreateTab { .. }
        | Command::CreatePane { .. }
        | Command::RenameWorkspace { .. }
        | Command::SetPinnedDirectory { .. }
        | Command::RepairPaneDirectory { .. }
        | Command::RetryPaneDirectory { .. }
        | Command::SetWorktreeLocation { .. }
        | Command::AssessClose { .. }
        | Command::CloseConfirmed { .. }
        | Command::KillWorkspaceConfirmed { .. }
        | Command::AssessEndAllSessions { .. }
        | Command::CompareAcquire(_)
        | Command::TakeOver(_)
        | Command::BeginStartup(_)
        | Command::CompleteStartup(_)
        | Command::CompleteMeasuredStartup(_)
        | Command::CancelStartup { .. }
        | Command::RefreshWorktrees { .. }
        | Command::CreateWorktree { .. }
        | Command::AssessRemoveWorktree { .. }
        | Command::RemoveWorktreeConfirmed { .. }
        | Command::AssessKillWorkspace { .. }
        | Command::SetRemoteAccess { .. }
        | Command::Worktree(_)
        | Command::Pairing(
            PairingCommand::IssueDirectAuthorization { .. }
            | PairingCommand::AssessUnpair { .. }
            | PairingCommand::BindAdditionalClient { .. },
        ) => ReservationClass::Ordinary,
    })
}

pub fn command_pending_kind(command: &Command) -> PendingKind {
    match command {
        Command::CloseConfirmed { .. }
        | Command::KillWorkspaceConfirmed { .. }
        | Command::EndAllSessions { .. } => PendingKind::External {
            effect: ExternalEffectKind::SessionLifecycle,
        },
        Command::CreateWorktree { .. }
        | Command::RemoveWorktreeConfirmed { .. }
        | Command::Worktree(_) => PendingKind::External {
            effect: ExternalEffectKind::GitWrite,
        },
        Command::CreateWorkspace { .. }
        | Command::CreateTab { .. }
        | Command::CreatePane { .. }
        | Command::RenameWorkspace { .. }
        | Command::SetPinnedDirectory { .. }
        | Command::RepairPaneDirectory { .. }
        | Command::RetryPaneDirectory { .. }
        | Command::SetWorktreeLocation { .. }
        | Command::AssessClose { .. }
        | Command::AssessEndAllSessions { .. }
        | Command::CompareAcquire(_)
        | Command::TakeOver(_)
        | Command::ReleaseControl(_)
        | Command::BeginStartup(_)
        | Command::CompleteStartup(_)
        | Command::CompleteMeasuredStartup(_)
        | Command::CancelStartup { .. }
        | Command::RevokePairing { .. }
        | Command::RefreshWorktrees { .. }
        | Command::AssessRemoveWorktree { .. }
        | Command::AssessKillWorkspace { .. }
        | Command::SetRemoteAccess { .. }
        | Command::Pairing(_) => PendingKind::AtomicMutation,
    }
}

impl OperationLedger {
    pub fn prepare_register_stream(
        &mut self,
        binding: StreamBinding,
        context: TransactionContext,
    ) -> Result<PreparedLedgerTransition, LedgerFailure> {
        self.prepare_register_stream_with_purpose(binding, StreamPurpose::Ordinary, context)
    }
    pub fn prepare_register_stream_with_purpose(
        &mut self,
        binding: StreamBinding,
        purpose: StreamPurpose,
        context: TransactionContext,
    ) -> Result<PreparedLedgerTransition, LedgerFailure> {
        self.ready(context)?;
        if self.closed {
            return Err(failure(LedgerFailureClass::AdmissionsClosed));
        }
        Self::store_only(context)?;
        if binding.server() != self.server {
            return Err(failure(LedgerFailureClass::ScopeMismatch));
        }
        let record = if let Some(existing) = self.state.streams.get(&binding.stream().key()) {
            if existing.binding != binding {
                return Err(failure(LedgerFailureClass::ScopeMismatch));
            }
            if existing.purpose != purpose {
                return Err(failure(LedgerFailureClass::PurposeMismatch));
            }
            existing.clone()
        } else {
            let limits = self.state.limits;
            let count = self
                .state
                .streams
                .values()
                .filter(|v| {
                    v.binding.pairing() == binding.pairing()
                        && v.binding.client() == binding.client()
                })
                .count();
            if self.state.streams.len() >= limits.max_streams() as usize
                || count >= limits.max_streams_per_client_binding() as usize
            {
                return Err(failure(LedgerFailureClass::StreamLimit));
            }
            let dormant = match purpose {
                StreamPurpose::Ordinary => None,
                StreamPurpose::SelfRevocation => Some(OutcomeReservation::full(
                    ReservationClass::DedicatedSelfRevocation,
                )),
                StreamPurpose::Enrollment { .. } => Some(OutcomeReservation::full(
                    ReservationClass::DedicatedEnrollment,
                )),
            };
            if let Some(reservation) = dormant {
                self.check_capacity(binding, reservation.class())?;
            }
            StreamRecord::new(binding, purpose, StreamStatus::Active, 0, 0, dormant)?
        };
        self.prepare(
            context,
            vec![LedgerWrite::RegisterStream(record)],
            PreparedResolution::Registered,
        )
    }
    pub fn prepare_admission(
        &mut self,
        binding: &StreamBinding,
        operation: OperationId,
        command: &Command,
        pending: PendingKind,
        context: TransactionContext,
    ) -> Result<AdmissionPreparation, LedgerFailure> {
        self.ready(context)?;
        let header = self.header(binding)?;
        binding.check_operation(operation)?;
        let digest = super::canonical::canonical_request_digest(binding, operation, command)?;
        let lookup = self.lookup(binding, operation, Some(digest))?;
        if lookup != OperationLookup::NeverAdmitted {
            return Ok(AdmissionPreparation::Existing(lookup));
        }
        if self.closed {
            return Err(failure(LedgerFailureClass::AdmissionsClosed));
        }
        Self::store_only(context)?;
        if pending != command_pending_kind(command) {
            return Err(failure(LedgerFailureClass::InvalidRequest));
        }
        let class = reservation_class(header.purpose, command)?;
        if matches!(command, Command::RevokePairing { pairing } if header.purpose == StreamPurpose::SelfRevocation && *pairing != binding.pairing())
        {
            return Err(failure(LedgerFailureClass::PurposeMismatch));
        }
        if header.purpose == StreamPurpose::Ordinary {
            self.check_capacity(*binding, class)?;
        } else if operation.sequence().get() != 1
            || header.dormant_reservation != Some(OutcomeReservation::full(class))
        {
            return Err(failure(LedgerFailureClass::PurposeMismatch));
        }
        let resulting_store = context
            .expected_store()
            .checked_successor()
            .ok_or_else(|| failure(LedgerFailureClass::CounterExhausted))?;
        let op_binding = OperationBinding {
            id: operation,
            digest,
        };
        let external_progress = match pending {
            PendingKind::AtomicMutation => None,
            PendingKind::External { effect } => Some(PendingExternalProgress::new(
                PendingExternalProgressParts {
                    binding: op_binding,
                    effect,
                    state: ExternalProgressState::AwaitingIntent,
                },
            )?),
        };
        let record = PendingOperationRecord {
            binding: op_binding,
            kind: pending,
            reservation: OutcomeReservation::full(class),
            admitted_at: resulting_store,
            admission_organization_revision: context.base_organization(),
            progress_recorded_at: resulting_store,
            external_progress,
        };
        Ok(AdmissionPreparation::Prepared(self.prepare(
            context,
            vec![LedgerWrite::AdmitOperation(record)],
            PreparedResolution::Admitted(op_binding, pending),
        )?))
    }
    pub(super) fn check_capacity(
        &self,
        binding: StreamBinding,
        class: ReservationClass,
    ) -> Result<(), LedgerFailure> {
        let mut total = 0u32;
        let mut stream_total = 0u32;
        let mut ordinary = 0u32;
        let mut stream_ordinary = 0u32;
        for (stream, reserved) in self
            .state
            .streams
            .values()
            .filter_map(|v| v.dormant_reservation.map(|r| (v.binding.stream(), r)))
            .chain(
                self.state
                    .operations
                    .values()
                    .map(|v| (record_binding(v).id.stream(), reservation(v))),
            )
        {
            total += 1;
            if stream == binding.stream() {
                stream_total += 1;
            }
            if reserved.class() == ReservationClass::Ordinary {
                ordinary += 1;
                if stream == binding.stream() {
                    stream_ordinary += 1;
                }
            }
        }
        let limits = self.state.limits;
        let stream_full = stream_total >= limits.per_stream_slots()
            || (class == ReservationClass::Ordinary
                && stream_ordinary >= limits.ordinary_stream_slots());
        let global_full = total >= limits.global_slots()
            || (class == ReservationClass::Ordinary && ordinary >= limits.ordinary_global_slots());
        if stream_full || global_full {
            return Err(LedgerFailure::with_metadata(
                LedgerFailureClass::QuotaExceeded,
                LedgerFailureMetadata {
                    scope: Some(if stream_full {
                        LedgerQuotaScope::Stream
                    } else {
                        LedgerQuotaScope::Server
                    }),
                    resource: Some(LedgerResourceKind::Slots),
                    limit: Some(if stream_full {
                        limits.per_stream_slots()
                    } else {
                        limits.global_slots()
                    } as u64),
                    ..Default::default()
                },
            ));
        }
        Ok(())
    }
}
