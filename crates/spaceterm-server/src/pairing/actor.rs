//! Purpose-bound pairing entry points for the serialized actor.
//!
//! `execute_pairing` admits only a pre-registered stream whose immutable purpose
//! matches the canonical command. Enrollment and self-revocation never construct
//! an ordinary principal or enter ordinary submit, lookup or acknowledgement.
//! Ordinary pairing retains the exact authenticated principal and connection
//! generation through admission and planning, with reliable egress reserved first.
//! Store-only admission precedes the move-only planner. Planning, capacity checks,
//! receipt signing, principal allocation and activation run in one exclusive actor
//! window. The final transaction installs pairing writes, reducer grant releases,
//! stream registration/retirement and the original retained outcome together.
//!
//! Historical enrollment and self-revocation reads compare the original binding
//! and purpose. Acknowledgement compacts only that stream. Receipts remain in the
//! pairing projection after compaction and revocation. Ticket invalidation retires
//! prospective streams without reusing identities. Native owner issuance retains
//! its native request identity, not a manufactured ordinary operation identity.
//! Native admission and authority are supplied by the account-owner provider.
//! Server-owned unpair comparisons survive outcome acknowledgement, not restart.
//! They retain the exact issuer stream and assessment for at most 60 seconds after
//! durable completion, with at most 16 per Client and 256 total. Policy revalidates
//! the target authorization version and Client set. Historical replay cannot
//! refresh comparison authority. Deterministic admitted rejections retain a typed
//! terminal outcome; temporary provider failures leave the original entry pending.

use super::*;
use crate::pairing::service::{
    PairingActorRequest, PairingExecution, PairingMutation, PairingMutationContext,
};
use spaceterm_core::pairing::policy::{PairingPolicy, PairingSnapshot};
use spaceterm_protocol::pairing_adapter::EnrollmentAuthorization;

pub(crate) type PairingReply = oneshot::Sender<Result<PairingExecution, PairingFailure>>;
pub(crate) enum PairingHistory {
    Enrollment {
        ticket: EnrollmentTicketId,
        claim_digest: RequestDigest,
    },
    SelfRevocation(PairingTarget),
}
pub(crate) enum PairingActorMessage {
    Execute {
        request: Box<PairingActorRequest>,
        mutation: Box<dyn PairingMutation>,
        reply: PairingReply,
    },
    Status {
        binding: OperationBinding,
        history: PairingHistory,
        reply: Reply<Option<SignedPairingReceipt>>,
    },
    Acknowledge {
        binding: OperationBinding,
        history: PairingHistory,
        reply: Reply<CommitReceipt>,
    },
    Invalidate(Reply<CommitReceipt>),
    Outcome {
        binding: OperationBinding,
        reply: Reply<(OrganizationRevision, PairingOutcome)>,
    },
    Connections {
        server: ServerId,
        epoch: ServerEpoch,
        authenticator: crate::connection::Authenticator,
    },
}
impl PairingActorMessage {
    pub(crate) fn reject(self, error: ServerFailure) {
        match self {
            Self::Connections { .. } => (),
            Self::Outcome { reply, .. } => {
                let _ = reply.send(Err(error));
            }
            Self::Execute { reply, .. } => {
                let _ = reply.send(Err(pairing_failure(error)));
            }
            Self::Status { reply, .. } => {
                let _ = reply.send(Err(error));
            }
            Self::Acknowledge { reply, .. } | Self::Invalidate(reply) => {
                let _ = reply.send(Err(error));
            }
        }
    }
}
pub(crate) fn pairing_failure(error: ServerFailure) -> PairingFailure {
    match error.class {
        ServerFailureClass::Capacity | ServerFailureClass::Exhausted => {
            PairingFailure::LimitExceeded
        }
        ServerFailureClass::StorageUnavailable => PairingFailure::StorageUnavailable,
        ServerFailureClass::RecoveryRequired | ServerFailureClass::CleanupIncomplete => {
            PairingFailure::RecoveryRequired
        }
        ServerFailureClass::Authentication => PairingFailure::AuthorizationDenied,
        ServerFailureClass::ScopeMismatch => PairingFailure::IdentityMismatch,
        ServerFailureClass::StaleAuthority => PairingFailure::StaleAssessment,
        _ => PairingFailure::AuthorizationUnavailable,
    }
}
struct MutationContext<'a> {
    snapshot: PairingSnapshot,
    now: u64,
    ledger: &'a dyn LedgerPort,
    context: TransactionContext,
}
impl PairingMutationContext for MutationContext<'_> {
    fn snapshot(&self) -> &PairingSnapshot {
        &self.snapshot
    }
    fn now_millis(&self) -> u64 {
        self.now
    }
    fn reserve_stream_capacity(&mut self, streams: &[StreamRecord]) -> Result<(), PairingFailure> {
        self.ledger
            .check_pairing_capacity(streams, self.context)
            .map_err(pairing_failure)
    }
}
impl OperationReply {
    fn pairing(sender: PairingReply, permit: Option<Box<dyn ReliableOutcomePermit>>) -> Self {
        Self {
            sender: OperationReplySender::Pairing {
                sender,
                authorization: None,
            },
            permit,
        }
    }
    fn authorization(&mut self, value: Option<EnrollmentAuthorization>) {
        if let OperationReplySender::Pairing { authorization, .. } = &mut self.sender {
            *authorization = value;
        }
    }
    fn pairing_error(self, error: PairingFailure) {
        match self.sender {
            OperationReplySender::Pairing { sender, .. } => {
                let _ = sender.send(Err(error));
            }
            OperationReplySender::Ordinary(sender) => {
                let _ = sender.send(Err(crate::pairing::service::server_failure(error)));
            }
        }
    }
}
impl Actor {
    pub(super) fn pairing_record(
        &self,
        principal: VerifiedPrincipal,
    ) -> Result<spaceterm_protocol::authentication::PairingRecord, ServerFailure> {
        use spaceterm_protocol::authentication::PairingLookup;
        if self.pairing_state.borrow().is_some() {
            crate::pairing::protocol_lookup::PairingProtocolLookup::new(self.handle.clone())
                .lookup(
                    principal.server().bytes(),
                    encode_local(principal.pairing().local()),
                    encode_local(principal.client().local()),
                )
                .map_err(|_| failure(ServerFailureClass::Authentication))
        } else {
            self.pairing
                .lookup(
                    principal.server().bytes(),
                    encode_local(principal.pairing().local()),
                    encode_local(principal.client().local()),
                )
                .map_err(|_| failure(ServerFailureClass::Authentication))
        }
    }
    fn pairing_snapshot(&self) -> Result<PairingSnapshot, PairingFailure> {
        PairingSnapshot::new(
            self.cursor.server,
            self.cursor.epoch,
            self.cursor.store,
            self.cursor.high_water,
            self.pairing_state
                .borrow()
                .as_ref()
                .ok_or(PairingFailure::InvalidDurableState)?
                .recovery()
                .clone(),
        )
    }
    pub(super) fn install_pairing_writes(
        &mut self,
        parts: &TransactionParts,
    ) -> Result<(), ServerFailure> {
        let Some(old) = self.pairing_state.borrow().clone() else {
            return Ok(());
        };
        // Install the exact projection only after the complete validator and the
        // physical store have checked it and the actor has accepted the receipt.
        let mut recovery = old.recovery().parts().clone();
        for write in &parts.operation_writes {
            if let OperationWrite::Pairing(write) = write {
                match write.as_ref() {
                    PairingWrite::PutAuthorization { value, .. } => {
                        if let Some(old) = recovery
                            .authorizations
                            .iter_mut()
                            .find(|old| old.parts().record.id == value.parts().record.id)
                        {
                            *old = value.clone();
                        } else {
                            recovery.authorizations.push(value.clone());
                        }
                    }
                    PairingWrite::PutEnrollmentReservation { value, .. } => {
                        if let Some(old) = recovery
                            .enrollments
                            .iter_mut()
                            .find(|old| old.parts().ticket == value.parts().ticket)
                        {
                            *old = value.clone();
                        } else {
                            recovery.enrollments.push(value.clone());
                        }
                    }
                }
            }
        }
        let recovery = PairingRecovery::new(recovery)
            .map_err(|_| failure(ServerFailureClass::RecoveryRequired))?;
        self.pairing_state.send_replace(Some(
            PairingSnapshot::new(
                parts.server,
                parts.resulting_epoch,
                parts.resulting_store_revision,
                parts.resulting_high_water,
                recovery,
            )
            .map_err(crate::pairing::service::server_failure)?,
        ));
        Ok(())
    }
    pub(super) fn pairing_request(&mut self, message: PairingActorMessage) {
        match message {
            PairingActorMessage::Outcome { binding, reply } => {
                let result = self.lookup(binding).and_then(|response| {
                    if let OperationResponse::Completed {
                        revision,
                        outcome: Outcome::Pairing(outcome),
                        ..
                    } = response
                    {
                        Ok((revision, *outcome))
                    } else {
                        Err(failure(ServerFailureClass::StaleAuthority))
                    }
                });
                let _ = reply.send(result);
            }
            PairingActorMessage::Connections {
                server,
                epoch,
                authenticator,
            } => {
                if server != self.cursor.server
                    || epoch != self.cursor.epoch
                    || self.pairing_connections.len() >= self.config.limits.connections
                {
                    self.enter_recovery();
                } else {
                    self.pairing_connections.push(authenticator);
                }
            }
            PairingActorMessage::Execute {
                request,
                mutation,
                reply,
            } => {
                let execution = PairingActorMessage::Execute {
                    request,
                    mutation,
                    reply,
                };
                if let Some(PairingActorMessage::Execute {
                    request,
                    mutation,
                    reply,
                }) = self.invalidate_tickets(Some(execution), None)
                {
                    self.execute_pairing(*request, mutation, reply);
                }
            }
            PairingActorMessage::Status {
                binding,
                history,
                reply,
            } => {
                let result = self
                    .pairing_history(binding, &history)
                    .map_err(crate::pairing::service::server_failure);
                let _ = reply.send(result);
            }
            PairingActorMessage::Acknowledge {
                binding,
                history,
                reply,
            } => {
                let result = (|| {
                    self.available()?;
                    if self
                        .pairing_history(binding, &history)
                        .map_err(crate::pairing::service::server_failure)?
                        .is_none()
                    {
                        return Err(failure(ServerFailureClass::StaleAuthority));
                    }
                    // Another device's revocation is retained proof, not an admitted
                    // result on this stream. Never fabricate a prefix to acknowledge it.
                    let header = self
                        .config
                        .core
                        .ledger
                        .pairing_stream(StreamBinding::from_stream(binding.id.stream()))?;
                    if header.admitted_through == 0 {
                        return Ok(None);
                    }
                    let mut parts = self.cursor.parts().map_err(store_failure)?;
                    self.config
                        .core
                        .ledger
                        .prepare_acknowledgement(
                            StreamBinding::from_stream(binding.id.stream()),
                            binding.id.sequence(),
                            self.cursor.context().map_err(ledger_failure)?,
                        )?
                        .append_to(&mut parts)
                        .map_err(ledger_failure)?;
                    Ok(Some(parts))
                })();
                match result {
                    Ok(Some(parts)) => self.commit(parts, CommitAction::Acknowledged(reply)),
                    Ok(None) => {
                        let _ = reply.send(Ok(CommitReceipt {
                            store_revision: self.cursor.store,
                            organization_revision: self.cursor.organization,
                        }));
                    }
                    Err(error) => {
                        if self.pending.is_none() && !self.busy {
                            self.abort_preparation();
                        }
                        let _ = reply.send(Err(error));
                    }
                }
            }
            PairingActorMessage::Invalidate(reply) => {
                let _ = self.invalidate_tickets(None, Some(reply));
            }
        }
    }
    fn execute_pairing(
        &mut self,
        request: PairingActorRequest,
        mutation: Box<dyn PairingMutation>,
        sender: PairingReply,
    ) {
        match request {
            PairingActorRequest::Operation(request) => {
                self.execute_pairing_operation(None, request, mutation, sender);
            }
            PairingActorRequest::Ordinary { principal, request } => {
                self.execute_pairing_operation(Some(principal), request, mutation, sender);
            }
            PairingActorRequest::AccountOwner {
                request,
                digest,
                proposal,
            } => {
                let result = (|| {
                    self.available().map_err(pairing_failure)?;
                    if !self.ordinary_open() || self.terminal_barrier {
                        return Err(PairingFailure::AuthorizationUnavailable);
                    }
                    let state = self.pairing_snapshot()?;
                    let claim =
                        spaceterm_protocol::pairing_adapter::enrollment_proposal_digest(&proposal)
                            .map_err(|_| PairingFailure::InvalidRequest)?;
                    if let Some(e) = state.recovery().parts().enrollments.iter().find(|e| matches!(&e.parts().issuer,
                        EnrollmentIssuer::AccountOwner { native_request, .. } if native_request == request.as_bytes())) {
                        if !matches!(&e.parts().issuer, EnrollmentIssuer::AccountOwner { native_request_digest, .. } if *native_request_digest == digest)
                            || e.parts().claim_digest != claim { return Err(PairingFailure::RequestIdentityReused); }
                        return Ok(Err(PairingExecution { outcome: reserved_outcome(e)?, authorization: None }));
                    }
                    let prepared = mutation.prepare(&mut self.mutation_context()?)?;
                    let transition = prepared.transition;
                    if !matches!(
                        transition.outcome,
                        PairingOutcome::AuthorizationReserved { .. }
                    ) || !transition.release_clients.is_empty()
                    {
                        return Err(PairingFailure::InvalidRequest);
                    }
                    let mut parts = self
                        .cursor
                        .parts()
                        .map_err(store_failure)
                        .map_err(pairing_failure)?;
                    let reduction = self
                        .config
                        .core
                        .reducer
                        .prepare_pairing_mutation(None, transition.high_water, &[], self.cursor)
                        .map_err(pairing_failure)?;
                    reduction.append_to(&mut parts).map_err(pairing_failure)?;
                    crate::pairing::store_projection::append_pairing_writes(
                        &mut parts,
                        &transition,
                    )?;
                    self.config
                        .core
                        .ledger
                        .prepare_pairing_stream_changes(
                            &transition.register_streams,
                            &[],
                            self.cursor
                                .context()
                                .map_err(ledger_failure)
                                .map_err(pairing_failure)?,
                        )
                        .map_err(pairing_failure)?
                        .append_to(&mut parts)
                        .map_err(ledger_failure)
                        .map_err(pairing_failure)?;
                    Ok(Ok((
                        parts,
                        PairingExecution {
                            outcome: transition.outcome,
                            authorization: prepared.authorization,
                        },
                    )))
                })();
                match result {
                    Ok(Ok((parts, execution))) => self.commit(
                        parts,
                        CommitAction::PairingReserved {
                            execution,
                            reply: sender,
                        },
                    ),
                    Ok(Err(execution)) => {
                        let _ = sender.send(Ok(execution));
                    }
                    Err(error) => {
                        if self.pending.is_none() && !self.busy {
                            self.abort_preparation();
                        }
                        let _ = sender.send(Err(error));
                    }
                }
            }
        }
    }
    fn execute_pairing_operation(
        &mut self,
        principal: Option<VerifiedPrincipal>,
        request: CanonicalOperationRequest,
        mutation: Box<dyn PairingMutation>,
        sender: PairingReply,
    ) {
        let permit = match principal
            .map(|principal| self.config.outcomes.reserve(principal, request.binding()))
            .transpose()
        {
            Ok(permit) => permit,
            Err(error) => {
                let _ = sender.send(Err(pairing_failure(error)));
                return;
            }
        };
        let reply = OperationReply::pairing(sender, permit);
        let checked = (|| {
            if *self.phase.borrow() == ServicePhase::RecoveryRequired {
                return Err(PairingFailure::RecoveryRequired);
            }
            self.check_pairing_purpose(&request)?;
            self.check_pairing_connection(principal, &request, false)?;
            self.config
                .core
                .ledger
                .lookup(request.binding())
                .map_err(pairing_failure)
        })();
        match checked {
            Ok(OperationLookup::Completed {
                result,
                publication_revision,
            }) => {
                if self.delivery_pending == Some(request.binding()) {
                    reply.pairing_error(PairingFailure::StorageUnavailable);
                } else {
                    reply.send(Ok(OperationResponse::Completed {
                        binding: request.binding(),
                        revision: publication_revision,
                        outcome: result,
                    }));
                }
            }
            Ok(OperationLookup::Pending) => {
                self.prepare_pairing_mutation(principal, request, mutation, reply)
            }
            Ok(OperationLookup::NeverAdmitted) => {
                let result = (|| {
                    self.available().map_err(pairing_failure)?;
                    self.check_pairing_connection(principal, &request, true)?;
                    self.pairing_admission_authority(&request)?;
                    let mut parts = self
                        .cursor
                        .parts()
                        .map_err(store_failure)
                        .map_err(pairing_failure)?;
                    self.config
                        .core
                        .ledger
                        .prepare_admission(
                            &request,
                            PendingKind::AtomicMutation,
                            self.cursor
                                .context()
                                .map_err(ledger_failure)
                                .map_err(pairing_failure)?,
                        )
                        .map_err(pairing_failure)?
                        .append_to(&mut parts)
                        .map_err(ledger_failure)
                        .map_err(pairing_failure)?;
                    Ok(parts)
                })();
                match result {
                    Ok(parts) => {
                        self.busy = true;
                        self.commit(
                            parts,
                            CommitAction::PairingAdmission {
                                principal,
                                request,
                                mutation,
                                reply,
                            },
                        );
                    }
                    Err(error) => {
                        if self.pending.is_none() && !self.busy {
                            self.abort_preparation();
                        }
                        reply.pairing_error(error);
                    }
                }
            }
            Ok(OperationLookup::AcknowledgedRetired) => {
                reply.pairing_error(PairingFailure::AuthorizationConsumed)
            }
            Ok(_) => reply.pairing_error(PairingFailure::RequestIdentityReused),
            Err(error) => reply.pairing_error(error),
        }
    }
    fn check_pairing_connection(
        &self,
        principal: Option<VerifiedPrincipal>,
        request: &CanonicalOperationRequest,
        effects: bool,
    ) -> Result<(), PairingFailure> {
        match request.command() {
            Command::Pairing(PairingCommand::Enroll { .. } | PairingCommand::SelfRevoke { .. })
                if principal.is_none() =>
            {
                Ok(())
            }
            Command::Pairing(
                PairingCommand::IssueDirectAuthorization { .. }
                | PairingCommand::BindAdditionalClient { .. }
                | PairingCommand::AssessUnpair { .. }
                | PairingCommand::UnpairConfirmed { .. },
            ) => {
                let principal = principal.ok_or(PairingFailure::AuthorizationDenied)?;
                self.check_principal(principal, effects)
                    .map_err(pairing_failure)?;
                self.check_binding(principal, request.binding())
                    .map_err(pairing_failure)
            }
            _ => Err(PairingFailure::AuthorizationDenied),
        }
    }
    fn mutation_context(&self) -> Result<MutationContext<'_>, PairingFailure> {
        Ok(MutationContext {
            snapshot: self.pairing_snapshot()?,
            now: self.clock.now().map_err(pairing_failure)?.get(),
            ledger: self.config.core.ledger.as_ref(),
            context: self
                .cursor
                .context()
                .map_err(ledger_failure)
                .map_err(pairing_failure)?,
        })
    }
    pub(super) fn prepare_pairing_mutation(
        &mut self,
        principal: Option<VerifiedPrincipal>,
        request: CanonicalOperationRequest,
        mutation: Box<dyn PairingMutation>,
        mut reply: OperationReply,
    ) {
        let binding = request.binding();
        if let Err(error) = self.available() {
            reply.pairing_error(pairing_failure(error));
            return;
        }
        let result = (|| {
            self.check_pairing_purpose(&request)?;
            self.check_pairing_connection(principal, &request, true)?;
            self.pairing_admission_authority(&request)?;
            let now = self.clock.now().map_err(pairing_failure)?.get();
            self.pairing_assessments
                .retain(|_, (_, expires)| now < *expires);
            match request.command() {
                Command::Pairing(PairingCommand::AssessUnpair { .. }) => {
                    let limits = spaceterm_core::reducer::OrganizationLimits::default();
                    if self.pairing_assessments.len() >= limits.assessments
                        || self
                            .pairing_assessments
                            .keys()
                            .filter(|id| id.stream().client() == binding.id.stream().client())
                            .count()
                            >= limits.assessments_per_client
                    {
                        return Err(PairingFailure::LimitExceeded);
                    }
                }
                Command::Pairing(PairingCommand::UnpairConfirmed { assessment }) => {
                    let reference = assessment.parts().reference;
                    if reference.operation.stream() != binding.id.stream()
                        || self
                            .pairing_assessments
                            .get(&reference.operation)
                            .is_none_or(|(original, _)| original != assessment)
                    {
                        return Err(PairingFailure::StaleAssessment);
                    }
                }
                _ => (),
            }
            let prepared = mutation.prepare(&mut self.mutation_context()?)?;
            let transition = prepared.transition;
            let reduction = self
                .config
                .core
                .reducer
                .prepare_pairing_mutation(
                    Some(binding),
                    transition.high_water,
                    &transition.release_clients,
                    self.cursor,
                )
                .map_err(pairing_failure)?;
            let mut parts = self
                .cursor
                .parts()
                .map_err(store_failure)
                .map_err(pairing_failure)?;
            reduction.append_to(&mut parts).map_err(pairing_failure)?;
            crate::pairing::store_projection::append_pairing_writes(&mut parts, &transition)?;
            let basis = OperationReductionBasis::new(OperationReductionBasisParts {
                binding,
                base: self.cursor.organization,
                resulting: parts.resulting_organization_revision,
            })
            .map_err(ledger_failure)
            .map_err(pairing_failure)?;
            parts
                .operation_writes
                .push(OperationWrite::ReductionBasis(basis));
            let outcome = Outcome::Pairing(Box::new(transition.outcome));
            let retired: Vec<_> = transition
                .retire_ordinary_streams
                .iter()
                .copied()
                .map(StreamBinding::from_stream)
                .collect();
            let context = TransactionContext::new(
                self.cursor.store,
                self.cursor.organization,
                parts.resulting_organization_revision,
                Some(basis),
            )
            .map_err(ledger_failure)
            .map_err(pairing_failure)?;
            self.config
                .core
                .ledger
                .prepare_pairing_completion(
                    binding,
                    &transition.register_streams,
                    &retired,
                    &outcome,
                    parts.resulting_organization_revision,
                    context,
                )
                .map_err(pairing_failure)?
                .append_to(&mut parts)
                .map_err(ledger_failure)
                .map_err(pairing_failure)?;
            let mut reduction = reduction;
            reduction.outcome = outcome;
            Ok((
                parts,
                reduction,
                transition.writes,
                transition.release_clients,
                prepared.authorization,
            ))
        })();
        match result {
            Ok((parts, reduction, writes, revoked_clients, authorization)) => {
                reply.authorization(authorization);
                self.begin_pairing_commit(
                    parts,
                    CommitAction::PairingCompleted {
                        binding,
                        reduction,
                        writes,
                        revoked_clients,
                        reply,
                    },
                );
            }
            Err(error) => {
                self.abort_preparation();
                if deterministic_rejection(error) {
                    self.reject_pairing_admitted(binding, error, reply);
                } else {
                    reply.pairing_error(error);
                }
            }
        }
    }
    fn reject_pairing_admitted(
        &mut self,
        binding: OperationBinding,
        error: PairingFailure,
        reply: OperationReply,
    ) {
        match self.config.core.reducer.prepare_pairing_mutation(
            Some(binding),
            self.cursor.high_water,
            &[],
            self.cursor,
        ) {
            Ok(mut reduction) => {
                reduction.outcome = Outcome::Pairing(Box::new(PairingOutcome::Failed(error)));
                self.complete_reduction(binding, reduction, reply, None);
            }
            Err(error) => {
                self.abort_preparation();
                reply.send(Err(error));
            }
        }
    }
    pub(super) fn retain_pairing_assessment(
        &mut self,
        outcome: &Outcome,
    ) -> Result<(), ServerFailure> {
        if let Outcome::Pairing(outcome) = outcome
            && let PairingOutcome::UnpairAssessed(assessment) = outcome.as_ref()
        {
            // Comparison facts are independent of compact outcome acknowledgement.
            // Like destructive reducer assessments, they are bounded, expire and
            // are deliberately lost across restart. Historical replay never refreshes them.
            let now = self.clock.now()?.get();
            let limits = spaceterm_core::reducer::OrganizationLimits::default();
            self.pairing_assessments
                .retain(|_, (_, expires)| now < *expires);
            let operation = assessment.parts().reference.operation;
            if self.pairing_assessments.contains_key(&operation)
                || self.pairing_assessments.len() >= limits.assessments
                || self
                    .pairing_assessments
                    .keys()
                    .filter(|id| id.stream().client() == operation.stream().client())
                    .count()
                    >= limits.assessments_per_client
            {
                return Err(failure(ServerFailureClass::RecoveryRequired));
            }
            let expires = now
                .checked_add(limits.assessment_lifetime_ms)
                .ok_or_else(|| failure(ServerFailureClass::Exhausted))?;
            self.pairing_assessments.insert(
                assessment.parts().reference.operation,
                (assessment.clone(), expires),
            );
        }
        Ok(())
    }
    fn check_pairing_purpose(
        &self,
        request: &CanonicalOperationRequest,
    ) -> Result<(), PairingFailure> {
        let binding = request.binding();
        CanonicalOperationRequest::new(
            binding.id,
            request.command().clone(),
            self.config.core.codec.as_ref(),
        )
        .map_err(|_| PairingFailure::InvalidRequest)?
        .verify_binding(binding)
        .map_err(|_| PairingFailure::RequestIdentityReused)?;
        let header = self
            .config
            .core
            .ledger
            .pairing_stream(StreamBinding::from_stream(binding.id.stream()))
            .map_err(pairing_failure)?;
        match (header.purpose, request.command()) {
            (
                StreamPurpose::Enrollment { claim_digest },
                Command::Pairing(PairingCommand::Enroll {
                    claim_digest: actual,
                    ticket,
                }),
            ) if claim_digest == *actual => {
                let state = self.pairing_snapshot()?;
                let e = state.reservation(ticket)?.parts();
                if e.enrollment_stream != binding.id.stream()
                    || e.enrollment_request_digest != binding.digest
                    || binding.id.sequence().get() != 1
                {
                    return Err(PairingFailure::RequestIdentityReused);
                }
            }
            (
                StreamPurpose::SelfRevocation,
                Command::Pairing(PairingCommand::SelfRevoke { target }),
            ) => {
                self.check_self_binding(binding, target)?;
            }
            (
                StreamPurpose::Ordinary,
                Command::Pairing(
                    PairingCommand::IssueDirectAuthorization { .. }
                    | PairingCommand::BindAdditionalClient { .. }
                    | PairingCommand::AssessUnpair { .. }
                    | PairingCommand::UnpairConfirmed { .. },
                ),
            ) => (),
            _ => return Err(PairingFailure::AuthorizationDenied),
        }
        Ok(())
    }
    fn pairing_admission_authority(
        &self,
        request: &CanonicalOperationRequest,
    ) -> Result<(), PairingFailure> {
        if !self.ordinary_open() || self.terminal_barrier {
            return Err(PairingFailure::AuthorizationUnavailable);
        }
        let binding = request.binding();
        let header = self
            .config
            .core
            .ledger
            .pairing_stream(StreamBinding::from_stream(binding.id.stream()))
            .map_err(pairing_failure)?;
        if header.status != StreamStatus::Active {
            return Err(PairingFailure::PairingRevoked);
        }
        let state = self.pairing_snapshot()?;
        match request.command() {
            Command::Pairing(PairingCommand::Enroll { ticket, .. }) => {
                let e = state.reservation(ticket)?.parts();
                if e.state.kind() != EnrollmentReservationKind::Reserved
                    || e.issued_epoch != self.cursor.epoch
                    || self.clock.now().map_err(pairing_failure)?.get() >= e.expires_at_millis
                {
                    return Err(PairingFailure::AuthorizationExpired);
                }
            }
            _ => {
                if self.revoked.contains(&binding.id.stream().client())
                    || self.pairing_barrier.contains(&binding.id.stream().client())
                {
                    return Err(PairingFailure::PairingRevoked);
                }
                PairingPolicy::default().authorize_client(
                    &state,
                    binding.id.stream().pairing(),
                    binding.id.stream().client(),
                )?;
            }
        }
        Ok(())
    }
    fn check_self_binding(
        &self,
        binding: OperationBinding,
        target: &PairingTarget,
    ) -> Result<(), PairingFailure> {
        let state = self.pairing_snapshot()?;
        let a = state.authorization(target.parts().pairing)?.parts();
        if target.parts().server != state.server()
            || a.record.device != target.parts().device
            || binding.id.sequence().get() != 1
            || !a
                .clients
                .iter()
                .any(|client| client.parts().self_revocation == binding.id.stream())
        {
            return Err(PairingFailure::ClientNotBound);
        }
        let request = CanonicalOperationRequest::new(
            binding.id,
            Command::Pairing(PairingCommand::SelfRevoke {
                target: target.clone(),
            }),
            self.config.core.codec.as_ref(),
        )
        .map_err(|_| PairingFailure::InvalidRequest)?;
        if request.binding() != binding {
            return Err(PairingFailure::RequestIdentityReused);
        }
        Ok(())
    }
    fn pairing_history(
        &self,
        binding: OperationBinding,
        history: &PairingHistory,
    ) -> Result<Option<SignedPairingReceipt>, PairingFailure> {
        if *self.phase.borrow() == ServicePhase::RecoveryRequired {
            return Err(PairingFailure::RecoveryRequired);
        }
        if self.delivery_pending == Some(binding) || self.busy {
            return Err(PairingFailure::StorageUnavailable);
        }
        let header = self
            .config
            .core
            .ledger
            .pairing_stream(StreamBinding::from_stream(binding.id.stream()))
            .map_err(pairing_failure)?;
        let state = self.pairing_snapshot()?;
        match history {
            PairingHistory::Enrollment {
                ticket,
                claim_digest,
            } => {
                if header.purpose
                    != (StreamPurpose::Enrollment {
                        claim_digest: *claim_digest,
                    })
                {
                    return Err(PairingFailure::AuthorizationDenied);
                }
                PairingPolicy::default().completed_enrollment(
                    &state,
                    ticket,
                    binding,
                    *claim_digest,
                )
            }
            PairingHistory::SelfRevocation(target) => {
                if header.purpose != StreamPurpose::SelfRevocation {
                    return Err(PairingFailure::AuthorizationDenied);
                }
                self.check_self_binding(binding, target)?;
                Ok(state
                    .authorization(target.parts().pairing)?
                    .parts()
                    .revocation_receipt
                    .clone())
            }
        }
    }
    fn invalidate_tickets(
        &mut self,
        continuation: Option<PairingActorMessage>,
        reply: Option<Reply<CommitReceipt>>,
    ) -> Option<PairingActorMessage> {
        let prepared = (|| {
            self.available()?;
            let state = self
                .pairing_snapshot()
                .map_err(crate::pairing::service::server_failure)?;
            let invalidations = PairingPolicy::default()
                .prepare_ticket_invalidations(&state, self.clock.now()?.get())
                .map_err(crate::pairing::service::server_failure)?;
            if invalidations.writes.parts().writes.is_empty() {
                return Ok(None);
            }
            let mut parts = self.cursor.parts().map_err(store_failure)?;
            parts.operation_writes.extend(
                invalidations
                    .writes
                    .parts()
                    .writes
                    .iter()
                    .cloned()
                    .map(|write| OperationWrite::Pairing(Box::new(write))),
            );
            let retired: Vec<_> = invalidations
                .retire_streams
                .into_iter()
                .map(StreamBinding::from_stream)
                .collect();
            self.config
                .core
                .ledger
                .prepare_pairing_stream_changes(
                    &[],
                    &retired,
                    self.cursor.context().map_err(ledger_failure)?,
                )?
                .append_to(&mut parts)
                .map_err(ledger_failure)?;
            Ok(Some(parts))
        })();
        match prepared {
            Ok(Some(parts)) => {
                self.commit(
                    parts,
                    CommitAction::PairingInvalidated {
                        continuation,
                        reply,
                    },
                );
                None
            }
            Ok(None) => {
                if let Some(reply) = reply {
                    let _ = reply.send(Ok(CommitReceipt {
                        store_revision: self.cursor.store,
                        organization_revision: self.cursor.organization,
                    }));
                }
                continuation
            }
            Err(error) => {
                if self.pending.is_none() && !self.busy {
                    self.abort_preparation();
                }
                if let Some(reply) = reply {
                    let _ = reply.send(Err(error));
                }
                if let Some(message) = continuation {
                    message.reject(error);
                }
                None
            }
        }
    }
}
// These failures reject the admitted request, not the physical commit or a
// temporarily unavailable provider. Never strand their reserved ledger entries.
fn deterministic_rejection(error: PairingFailure) -> bool {
    matches!(
        error,
        PairingFailure::InvalidRequest
            | PairingFailure::LimitExceeded
            | PairingFailure::IdentityMismatch
            | PairingFailure::FingerprintMismatch
            | PairingFailure::AuthorizationRequired
            | PairingFailure::AuthorizationDenied
            | PairingFailure::PairingRevoked
            | PairingFailure::ClientNotBound
            | PairingFailure::StaleAssessment
            | PairingFailure::AuthorizationExpired
            | PairingFailure::AuthorizationConsumed
            | PairingFailure::RequestIdentityReused
    )
}
fn reserved_outcome(e: &EnrollmentReservation) -> Result<PairingOutcome, PairingFailure> {
    Ok(PairingOutcome::AuthorizationReserved {
        ticket: e.parts().ticket.clone(),
        enrollment: OperationId::new(
            e.parts().enrollment_stream,
            OperationSequence::new(1).map_err(|_| PairingFailure::InvalidDurableState)?,
        ),
        claim_digest: e.parts().claim_digest,
    })
}

pub(super) async fn invalidate_recovered_tickets(
    config: &mut ServerConfig,
    recovered: &mut crate::recovery::RecoveryResult,
    clock: &dyn ServerClock,
) -> Result<(), ServerFailure> {
    let Some(pairing) = recovered.recovered.pairing.clone() else {
        return Ok(());
    };
    let cursor = recovered.cursor;
    let state = PairingSnapshot::new(
        cursor.server,
        cursor.epoch,
        cursor.store,
        cursor.high_water,
        pairing,
    )
    .map_err(crate::pairing::service::server_failure)?;
    let invalidations = PairingPolicy::default()
        .prepare_ticket_invalidations(&state, clock.now()?.get())
        .map_err(crate::pairing::service::server_failure)?;
    if invalidations.writes.parts().writes.is_empty() {
        return Ok(());
    }
    let mut parts = cursor.parts().map_err(store_failure)?;
    parts.operation_writes.extend(
        invalidations
            .writes
            .parts()
            .writes
            .iter()
            .cloned()
            .map(|write| OperationWrite::Pairing(Box::new(write))),
    );
    let retired: Vec<_> = invalidations
        .retire_streams
        .into_iter()
        .map(StreamBinding::from_stream)
        .collect();
    config
        .core
        .ledger
        .prepare_pairing_stream_changes(&[], &retired, cursor.context().map_err(ledger_failure)?)?
        .append_to(&mut parts)
        .map_err(ledger_failure)?;
    let transaction = Arc::new(
        DurableTransaction::new_with_validation(parts, config.core.validator.as_ref())
            .map_err(store_failure)?,
    );
    let disposition = recovered
        .store
        .commit(transaction.clone())?
        .await
        .map_err(|_| failure(ServerFailureClass::RecoveryRequired))?;
    match disposition {
        CommitDisposition::Durable(receipt) => {
            recovered.cursor.install(transaction.parts(), receipt)?;
            config.core.reducer.committed(receipt)?;
            config.core.ledger.committed(receipt)?;
            recovered.recovered = recovered.store.recover().await?;
            Ok(())
        }
        CommitDisposition::NotApplied(error) => Err(store_failure(error)),
        CommitDisposition::Indeterminate => Err(failure(ServerFailureClass::RecoveryRequired)),
    }
}
