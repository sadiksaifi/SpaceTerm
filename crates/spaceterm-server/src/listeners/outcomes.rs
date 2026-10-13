//! Actor admission borrows the connection's already charged reliable reply slot.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use crate::{
    api::{OperationResponse, ReliableOutcomePermit, ReliableOutcomePort, VerifiedPrincipal},
    contracts::{ServerFailure, ServerFailureClass},
    egress::EgressReservation,
    failure::failure,
};
use spaceterm_core::operations::OperationBinding;

#[derive(Default)]
pub struct ConnectionOutcomes {
    slots: Mutex<Vec<(VerifiedPrincipal, Arc<ReplySlot>)>>,
}
struct ReplySlot {
    reservation: EgressReservation,
    active: AtomicBool,
    state: Mutex<ReplyState>,
}
#[derive(Default)]
struct ReplyState {
    binding: Option<OperationBinding>,
    published: Option<PublishedReply>,
}
impl ConnectionOutcomes {
    /// Creates the shared port to install in ServerConfig before recovery and
    /// in the production listener dispatcher afterward.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub(crate) fn begin(
        self: &Arc<Self>,
        principal: VerifiedPrincipal,
        reservation: EgressReservation,
    ) -> Result<ReplyScope, ServerFailure> {
        let mut slots = self
            .slots
            .lock()
            .map_err(|_| failure(ServerFailureClass::RecoveryRequired))?;
        if slots.iter().any(|(p, _)| *p == principal) {
            return Err(failure(ServerFailureClass::StaleAuthority));
        }
        if slots.len() >= crate::connection::MAX_AUTHENTICATED_CONNECTIONS {
            return Err(failure(ServerFailureClass::Capacity));
        }
        let slot = Arc::new(ReplySlot {
            reservation,
            active: AtomicBool::new(true),
            state: Mutex::new(ReplyState::default()),
        });
        slots.push((principal, slot.clone()));
        Ok(ReplyScope {
            owner: self.clone(),
            principal,
            slot,
        })
    }
}
impl ReliableOutcomePort for ConnectionOutcomes {
    fn reserve(
        &self,
        principal: VerifiedPrincipal,
        binding: OperationBinding,
    ) -> Result<Box<dyn ReliableOutcomePermit>, ServerFailure> {
        if binding.id.stream().server() != principal.server()
            || binding.id.stream().pairing() != principal.pairing()
            || binding.id.stream().client() != principal.client()
        {
            return Err(failure(ServerFailureClass::ScopeMismatch));
        }
        let slots = self
            .slots
            .lock()
            .map_err(|_| failure(ServerFailureClass::RecoveryRequired))?;
        let slot = slots
            .iter()
            .find(|(p, _)| *p == principal)
            .map(|(_, slot)| slot.clone())
            .ok_or_else(|| failure(ServerFailureClass::Capacity))?;
        let mut state = slot
            .state
            .lock()
            .map_err(|_| failure(ServerFailureClass::RecoveryRequired))?;
        if !slot.active.load(Ordering::Acquire) || state.binding.is_some() {
            return Err(failure(ServerFailureClass::StaleAuthority));
        }
        state.binding = Some(binding);
        drop(state);
        Ok(Box::new(OutcomePermit {
            _reservation: slot.reservation.retain(),
            slot,
            binding,
        }))
    }
}
struct OutcomePermit {
    slot: Arc<ReplySlot>,
    binding: OperationBinding,
    _reservation: EgressReservation,
}
impl ReliableOutcomePermit for OutcomePermit {
    fn publish(&mut self, response: &OperationResponse) -> Result<(), ServerFailure> {
        if !self.slot.active.load(Ordering::Acquire) {
            return Err(failure(ServerFailureClass::StaleAuthority));
        }
        match response {
            OperationResponse::Pending(binding) | OperationResponse::Completed { binding, .. }
                if *binding != self.binding =>
            {
                return Err(failure(ServerFailureClass::ScopeMismatch));
            }
            _ => {}
        }
        let mut state = self
            .slot
            .state
            .lock()
            .map_err(|_| failure(ServerFailureClass::RecoveryRequired))?;
        if state.published.is_some() {
            return Err(failure(ServerFailureClass::StaleAuthority));
        }
        state.published = Some(match response {
            OperationResponse::Pending(_) => PublishedReply::Pending,
            OperationResponse::Completed { revision, .. } => PublishedReply::Completed(*revision),
            OperationResponse::Retired => PublishedReply::Retired,
        });
        Ok(())
    }
}
#[derive(Clone, Copy)]
enum PublishedReply {
    Pending,
    Completed(spaceterm_core::authority::OrganizationRevision),
    Retired,
}

pub(crate) struct ReplyScope {
    owner: Arc<ConnectionOutcomes>,
    principal: VerifiedPrincipal,
    slot: Arc<ReplySlot>,
}
impl ReplyScope {
    /// The actor's publication and the outgoing unsigned reply must identify
    /// the same retained result. The original charge lasts through native send.
    pub(crate) fn validate_publication(
        &self,
        payload: &spaceterm_protocol::codec::Payload,
    ) -> Result<(), ServerFailure> {
        use spaceterm_protocol::{
            codec::Payload,
            wire::{v1, v2},
        };
        let state = self
            .slot
            .state
            .lock()
            .map_err(|_| failure(ServerFailureClass::RecoveryRequired))?;
        let Some(binding) = state.binding else {
            return Ok(());
        };
        let published = state
            .published
            .ok_or_else(|| failure(ServerFailureClass::RecoveryRequired))?;
        macro_rules! profile {
            ($message:expr, $p:ident) => {{
                match (&$message.kind, published) {
                    (Some($p::message::Kind::Outcome(value)), PublishedReply::Completed(revision)) => {
                        if value.publication_revision != revision.get() { return Err(failure(ServerFailureClass::ScopeMismatch)); }
                        value.operation.as_ref()
                    }
                    (Some($p::message::Kind::OperationLookupResult(value)), PublishedReply::Pending | PublishedReply::Retired) => {
                        let disposition_matches = matches!((&value.disposition, published),
                            (Some($p::operation_lookup_result::Disposition::Pending(true)), PublishedReply::Pending) |
                            (Some($p::operation_lookup_result::Disposition::AcknowledgedRetired(true)), PublishedReply::Retired));
                        if !disposition_matches || value.request_digest != binding.digest.bytes() { return Err(failure(ServerFailureClass::ScopeMismatch)); }
                        value.operation.as_ref()
                    }
                    _ => return Err(failure(ServerFailureClass::ScopeMismatch)),
                }
            }};
        }
        let operation = match payload {
            Payload::Major1(message) => profile!(message, v1),
            Payload::Major2(message) => profile!(message, v2),
        }
        .ok_or_else(|| failure(ServerFailureClass::ScopeMismatch))?;
        if spaceterm_protocol::convert::identity::decode_operation(operation)
            .map_err(crate::adapters::transport::protocol_failure)?
            != binding.id
        {
            return Err(failure(ServerFailureClass::ScopeMismatch));
        }
        Ok(())
    }
}
impl Drop for ReplyScope {
    fn drop(&mut self) {
        self.slot.active.store(false, Ordering::Release);
        if let Ok(mut slots) = self.owner.slots.lock() {
            slots.retain(|(principal, slot)| {
                *principal != self.principal || !Arc::ptr_eq(slot, &self.slot)
            });
        }
    }
}
