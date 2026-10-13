//! Production pairing bridge. The handle transfers a move-only planner to the
//! actor with the original ordinary principal or immutable restricted purpose,
//! then observes its durable result. Dropping this observer cannot cancel an
//! admitted mutation. Enrollment and self-revocation use immutable stream purpose,
//! not an ordinary principal. Only the verified pairing protocol service calls
//! these trusted entry points. Protocol proof verification remains at the listener.

use super::{protocol_lookup::PairingSnapshotPort, service::*};
use crate::{
    api::ServerHandle,
    contracts::{ServerFailure, ServerFuture},
    service::{ActorRequest, pairing_actor::*},
};
use spaceterm_core::{
    operations::{OperationBinding, RequestDigest},
    pairing::{policy::PairingSnapshot, *},
    storage::CommitReceipt,
};
use spaceterm_protocol::ProtocolError;
use tokio::sync::oneshot;

impl ServerHandle {
    pub fn pairing_snapshot(&self) -> Result<PairingSnapshot, PairingFailure> {
        if self.phase() == crate::api::ServicePhase::RecoveryRequired {
            return Err(PairingFailure::RecoveryRequired);
        }
        self.pairing_snapshot
            .borrow()
            .clone()
            .ok_or(PairingFailure::InvalidDurableState)
    }
    /// Purpose-bound durable admission followed by serialized full mutation
    /// planning. Ordinary requests retain the original principal and reserve reliable
    /// egress. Unknown streams and purpose escalation are rejected.
    pub async fn execute_pairing(
        &self,
        request: PairingActorRequest,
        mutation: Box<dyn PairingMutation>,
    ) -> Result<PairingExecution, PairingFailure> {
        let (reply, result) = oneshot::channel();
        self.send(ActorRequest::Pairing(PairingActorMessage::Execute {
            request: Box::new(request),
            mutation,
            reply,
        }))
        .map_err(pairing_failure)?;
        result.await.map_err(|_| PairingFailure::RecoveryRequired)?
    }
    /// Historical lookup needs the original ticket, claim and canonical binding.
    /// It grants no ordinary authorization and ignores consumed-ticket expiry.
    pub async fn enrollment_status(
        &self,
        binding: OperationBinding,
        ticket: EnrollmentTicketId,
        claim_digest: RequestDigest,
    ) -> Result<Option<SignedPairingReceipt>, PairingFailure> {
        self.pairing_status(
            binding,
            PairingHistory::Enrollment {
                ticket,
                claim_digest,
            },
        )
        .await
    }
    pub async fn enrollment_acknowledge(
        &self,
        binding: OperationBinding,
        ticket: EnrollmentTicketId,
        claim_digest: RequestDigest,
    ) -> Result<(), PairingFailure> {
        self.pairing_acknowledge(
            binding,
            PairingHistory::Enrollment {
                ticket,
                claim_digest,
            },
        )
        .await
    }
    /// Reads only the original target's receipt through its reserved stream.
    pub async fn self_revocation_status(
        &self,
        binding: OperationBinding,
        target: PairingTarget,
    ) -> Result<Option<SignedPairingReceipt>, PairingFailure> {
        self.pairing_status(binding, PairingHistory::SelfRevocation(target))
            .await
    }
    pub async fn self_revocation_acknowledge(
        &self,
        binding: OperationBinding,
        target: PairingTarget,
    ) -> Result<(), PairingFailure> {
        self.pairing_acknowledge(binding, PairingHistory::SelfRevocation(target))
            .await
    }
    /// Retires expired prospective streams and preserves non-reused identities.
    /// Recovery runs this before listeners; issuance runs it before reserving.
    pub async fn invalidate_pairing_tickets(&self) -> Result<CommitReceipt, ServerFailure> {
        let (reply, result) = oneshot::channel();
        self.send(ActorRequest::Pairing(PairingActorMessage::Invalidate(
            reply,
        )))?;
        crate::api::receive(result).await
    }
    pub(crate) async fn pairing_outcome(
        &self,
        binding: OperationBinding,
    ) -> Result<
        (
            spaceterm_core::authority::OrganizationRevision,
            PairingOutcome,
        ),
        ServerFailure,
    > {
        let (reply, result) = oneshot::channel();
        self.send(ActorRequest::Pairing(PairingActorMessage::Outcome {
            binding,
            reply,
        }))?;
        crate::api::receive(result).await
    }
    async fn pairing_status(
        &self,
        binding: OperationBinding,
        history: PairingHistory,
    ) -> Result<Option<SignedPairingReceipt>, PairingFailure> {
        let (reply, result) = oneshot::channel();
        self.send(ActorRequest::Pairing(PairingActorMessage::Status {
            binding,
            history,
            reply,
        }))
        .map_err(pairing_failure)?;
        crate::api::receive(result).await.map_err(pairing_failure)
    }
    async fn pairing_acknowledge(
        &self,
        binding: OperationBinding,
        history: PairingHistory,
    ) -> Result<(), PairingFailure> {
        let (reply, result) = oneshot::channel();
        self.send(ActorRequest::Pairing(PairingActorMessage::Acknowledge {
            binding,
            history,
            reply,
        }))
        .map_err(pairing_failure)?;
        crate::api::receive(result).await.map_err(pairing_failure)?;
        Ok(())
    }
}
impl PairingActorPort for ServerHandle {
    fn execute<'a>(
        &'a self,
        request: PairingActorRequest,
        mutation: Box<dyn PairingMutation>,
    ) -> ServerFuture<'a, Result<PairingExecution, PairingFailure>> {
        Box::pin(self.execute_pairing(request, mutation))
    }
    fn restricted_status<'a>(
        &'a self,
        binding: OperationBinding,
        target: PairingTarget,
    ) -> ServerFuture<'a, Result<Option<SignedPairingReceipt>, PairingFailure>> {
        Box::pin(self.self_revocation_status(binding, target))
    }
    fn enrollment_status<'a>(
        &'a self,
        binding: OperationBinding,
        ticket: EnrollmentTicketId,
        claim_digest: RequestDigest,
    ) -> ServerFuture<'a, Result<Option<SignedPairingReceipt>, PairingFailure>> {
        Box::pin(ServerHandle::enrollment_status(
            self,
            binding,
            ticket,
            claim_digest,
        ))
    }
    fn enrollment_acknowledge<'a>(
        &'a self,
        binding: OperationBinding,
        ticket: EnrollmentTicketId,
        claim_digest: RequestDigest,
    ) -> ServerFuture<'a, Result<(), PairingFailure>> {
        Box::pin(ServerHandle::enrollment_acknowledge(
            self,
            binding,
            ticket,
            claim_digest,
        ))
    }
    fn restricted_acknowledge<'a>(
        &'a self,
        binding: OperationBinding,
        target: PairingTarget,
    ) -> ServerFuture<'a, Result<(), PairingFailure>> {
        Box::pin(self.self_revocation_acknowledge(binding, target))
    }
}
impl PairingSnapshotPort for ServerHandle {
    fn snapshot(&self) -> Result<PairingSnapshot, ProtocolError> {
        self.pairing_snapshot()
            .map_err(|_| ProtocolError::PairingDenied)
    }
}
