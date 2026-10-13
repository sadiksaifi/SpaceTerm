//! Headless management uses the portable operation tracker and the existing
//! authenticated worker. No operation allocator, retry ledger or second route.
use spaceterm_client::{
    api::{ConnectedInfo, ConnectionState, PairedServerBinding},
    failure::{ClientFailure, ClientFailureClass},
    journal::JournalController,
    operations::{OperationPeer, OperationStatus, OperationTicket, OperationTracker},
    ports::{
        ClientFuture, ConnectionDelegate, ConnectionInvalidation, OutboundClass, ProtocolChannel,
    },
    registry::{ClientRegistry, ServerHandle},
    replica::ReplicaSnapshot,
};
use spaceterm_core::{
    identity::OperationStreamId,
    operation_ledger_contracts::CanonicalOperationRequest,
    operations::{
        Command, OperationBinding, OperationId, OperationLookup, OperationSequence, Outcome,
    },
};
use spaceterm_protocol::{
    codec::Payload,
    convert::{identity, operations},
    wire::{v1, v2},
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};
use tokio::sync::oneshot;

macro_rules! pairing_decode {
    ($p:ident, $value:expr, $info:expr, $codec:expr) => {
        pairing::$p::decode($value, $info, $codec)
    };
}
macro_rules! pairing_outcome {
    ($p:ident, $value:expr, $binding:expr) => {
        pairing::$p::outcome($value, $binding)
    };
}
pub(super) struct ManagementOperations {
    pub tracker: OperationTracker,
    binding: PairedServerBinding,
    registry: Mutex<Weak<ClientRegistry>>,
    lookups: Mutex<BTreeMap<OperationId, oneshot::Sender<Result<OperationLookup, ClientFailure>>>>,
}
impl ManagementOperations {
    pub fn new(journal: Arc<JournalController>, binding: PairedServerBinding) -> Self {
        Self {
            tracker: OperationTracker::new(journal),
            binding,
            registry: Mutex::default(),
            lookups: Mutex::default(),
        }
    }
    pub fn attach(&self, registry: &Arc<ClientRegistry>) -> Result<(), ClientFailure> {
        *self.registry.lock().map_err(|_| invalid())? = Arc::downgrade(registry);
        Ok(())
    }
    fn server(&self) -> Result<ServerHandle, ClientFailure> {
        self.registry
            .lock()
            .map_err(|_| invalid())?
            .upgrade()
            .and_then(|registry| registry.server(self.binding.target().server()))
            .ok_or_else(offline)
    }
    fn connected(&self) -> Result<(ServerHandle, ConnectedInfo), ClientFailure> {
        let server = self.server()?;
        let info = match *server.state().borrow() {
            ConnectionState::Connected { info } if server.replica().borrow().is_ready() => info,
            _ => return Err(offline()),
        };
        if !self.tracker.journal().ordinary_use_allowed(&self.binding)? {
            return Err(denied());
        }
        Ok((server, info))
    }
    pub async fn submit_command(&self, command: Command) -> Result<OperationTicket, ClientFailure> {
        self.tracker
            .submit(&self.binding, command, None, self)
            .await
    }
    pub async fn wait(&self, ticket: &OperationTicket) -> Result<Outcome, ClientFailure> {
        let mut status = ticket.status();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match status.borrow_and_update().clone() {
                    OperationStatus::Completed { outcome, .. } => return Ok(outcome),
                    OperationStatus::Retired => return Err(denied()),
                    _ => {}
                }
                status.changed().await.map_err(|_| offline())?;
            }
        })
        .await
        .map_err(|_| offline())?
    }
    pub async fn resolve_original(
        &self,
        operation: OperationId,
    ) -> Result<OperationTicket, ClientFailure> {
        let ticket = self.tracker.ticket(operation)?;
        let result = self.lookup(ticket.binding()).await?;
        self.tracker
            .resolve_bound(ticket.binding(), result, self)
            .await?;
        Ok(ticket)
    }
    fn record(&self, id: OperationId) -> Result<OperationBinding, ClientFailure> {
        if id.stream().client() != self.binding.target().client()
            || id.stream().pairing() != self.binding.target().pairing()
        {
            return Err(denied());
        }
        self.tracker
            .journal()
            .snapshot()?
            .operation(id)
            .map(|record| record.binding)
            .ok_or_else(invalid)
    }
    fn permitted(command: &Command) -> bool {
        matches!(
            command,
            Command::AssessEndAllSessions { .. }
                | Command::EndAllSessions { .. }
                | Command::SetRemoteAccess { .. }
                | Command::Pairing(spaceterm_core::pairing::PairingCommand::AssessUnpair { .. })
        )
    }
}
impl OperationPeer for ManagementOperations {
    fn validate_submission(
        &self,
        binding: &PairedServerBinding,
        command: &Command,
        workspace: Option<spaceterm_core::identity::WorkspaceId>,
    ) -> Result<(), ClientFailure> {
        if binding != &self.binding || workspace.is_some() || !Self::permitted(command) {
            return Err(denied());
        }
        self.connected()?;
        Ok(())
    }
    fn submit<'a>(&'a self, request: &'a CanonicalOperationRequest) -> ClientFuture<'a, ()> {
        Box::pin(async move {
            let (server, info) = self.connected()?;
            let request = self.tracker.admit_dispatch(request.binding())?;
            let payload = match info.major {
                1 => Payload::Major1(v1::Message {
                    kind: Some(v1::message::Kind::Operation(pairing::v1::encode(&request)?)),
                }),
                2 => Payload::Major2(v2::Message {
                    kind: Some(v2::message::Kind::Operation(pairing::v2::encode(&request)?)),
                }),
                _ => return Err(invalid()),
            };
            server.dispatch(info, payload).await
        })
    }
    fn lookup(&self, binding: OperationBinding) -> ClientFuture<'_, OperationLookup> {
        Box::pin(async move {
            if self.record(binding.id)? != binding {
                return Err(invalid());
            }
            let (server, info) = self.connected()?;
            let (reply, response) = oneshot::channel();
            {
                let mut lookups = self.lookups.lock().map_err(|_| invalid())?;
                lookups.retain(|_, reply| !reply.is_closed());
                if lookups.len() >= 4 || lookups.contains_key(&binding.id) {
                    return Err(ClientFailure::new(ClientFailureClass::ResourceLimit));
                }
                lookups.insert(binding.id, reply);
            }
            let payload = match info.major {
                1 => Payload::Major1(v1::Message {
                    kind: Some(v1::message::Kind::OperationLookupRequest(
                        operations::v1::encode_lookup_request(binding),
                    )),
                }),
                2 => Payload::Major2(v2::Message {
                    kind: Some(v2::message::Kind::OperationLookupRequest(
                        operations::v2::encode_lookup_request(binding),
                    )),
                }),
                _ => return Err(invalid()),
            };
            let result = tokio::time::timeout(Duration::from_secs(5), async {
                server.dispatch(info, payload).await?;
                response.await.map_err(|_| offline())?
            })
            .await
            .map_err(|_| offline())?;
            self.lookups
                .lock()
                .map_err(|_| invalid())?
                .remove(&binding.id);
            result
        })
    }
    fn acknowledge(&self, stream: OperationStreamId, through: u64) -> ClientFuture<'_, ()> {
        Box::pin(async move {
            let (server, info) = self.connected()?;
            let through = OperationSequence::new(through).map_err(|_| invalid())?;
            let payload = match info.major {
                1 => Payload::Major1(v1::Message {
                    kind: Some(v1::message::Kind::Acknowledgement(
                        operations::v1::encode_acknowledgement(stream, through),
                    )),
                }),
                2 => Payload::Major2(v2::Message {
                    kind: Some(v2::message::Kind::Acknowledgement(
                        operations::v2::encode_acknowledgement(stream, through),
                    )),
                }),
                _ => return Err(invalid()),
            };
            server.dispatch(info, payload).await
        })
    }
}
impl ConnectionDelegate for ManagementOperations {
    fn invalidate(&self, _: &PairedServerBinding, _: ConnectionInvalidation) {
        if let Ok(mut lookups) = self.lookups.lock() {
            let pending = std::mem::take(&mut *lookups);
            drop(lookups);
            for (_, reply) in pending {
                let _ = reply.send(Err(offline()));
            }
        }
    }
    fn reconcile<'a>(
        &'a self,
        _: ConnectedInfo,
        replica: &'a ReplicaSnapshot,
        _: &'a mut dyn ProtocolChannel,
    ) -> ClientFuture<'a, ()> {
        Box::pin(async move { self.tracker.reconcile_grants(&self.binding, replica).await })
    }
    fn publication(&self, _: ConnectedInfo, _: &ReplicaSnapshot) -> Result<(), ClientFailure> {
        Ok(())
    }
    fn admit(
        &self,
        info: ConnectedInfo,
        message: &Payload,
    ) -> Result<OutboundClass, ClientFailure> {
        if info.client != self.binding.target().client()
            || info.pairing != self.binding.target().pairing()
            || !self.tracker.journal().ordinary_use_allowed(&self.binding)?
        {
            return Err(denied());
        }
        macro_rules! check {
            ($m:expr,$p:ident) => {
                match &$m.kind {
                    Some($p::message::Kind::Operation(value)) => {
                        let request =
                            pairing_decode!($p, value, info, self.tracker.journal().codec())?;
                        let retained = self.tracker.admit_dispatch(request.binding())?;
                        if retained != request || !Self::permitted(request.command()) {
                            return Err(denied());
                        }
                        Ok(OutboundClass::Ordinary)
                    }
                    Some($p::message::Kind::OperationLookupRequest(value)) => {
                        let binding =
                            operations::$p::decode_lookup_request(value, info.pairing, info.client)
                                .map_err(|_| invalid())?;
                        if self.record(binding.id)? != binding {
                            return Err(invalid());
                        }
                        Ok(OutboundClass::Lifecycle)
                    }
                    Some($p::message::Kind::Acknowledgement(value)) => {
                        let (stream, through) = operations::$p::decode_acknowledgement(
                            value,
                            info.pairing,
                            info.client,
                        )
                        .map_err(|_| invalid())?;
                        let state = self.tracker.journal().snapshot()?;
                        let header = state.stream(stream).ok_or_else(invalid)?;
                        if through.get() > header.durable_prefix {
                            return Err(denied());
                        }
                        Ok(OutboundClass::Lifecycle)
                    }
                    _ => Err(denied()),
                }
            };
        }
        match message {
            Payload::Major1(m) => check!(m, v1),
            Payload::Major2(m) => check!(m, v2),
        }
    }
    fn receive<'a>(
        &'a self,
        info: ConnectedInfo,
        message: Payload,
        channel: &'a mut dyn ProtocolChannel,
    ) -> ClientFuture<'a, ()> {
        Box::pin(async move {
            let peer = OutcomePeer {
                channel: tokio::sync::Mutex::new(channel),
                info,
            };
            macro_rules! receive {
                ($m:expr,$p:ident) => {
                    match $m.kind {
                        Some($p::message::Kind::Outcome(value)) => {
                            let operation = identity::decode_operation(
                                value.operation.as_ref().ok_or_else(invalid)?,
                            )
                            .map_err(|_| invalid())?;
                            let binding = self.record(operation)?;
                            let (revision, result) = pairing_outcome!($p, &value, binding)?;
                            self.tracker
                                .resolve_bound(
                                    binding,
                                    OperationLookup::Completed {
                                        result,
                                        publication_revision: revision,
                                    },
                                    &peer,
                                )
                                .await
                        }
                        Some($p::message::Kind::OperationLookupResult(value)) => {
                            let operation = identity::decode_operation(
                                value.operation.as_ref().ok_or_else(invalid)?,
                            )
                            .map_err(|_| invalid())?;
                            let binding = self.record(operation)?;
                            let result = operations::$p::decode_lookup_result(&value, binding)
                                .map_err(|_| invalid())?;
                            let reply = self
                                .lookups
                                .lock()
                                .map_err(|_| invalid())?
                                .remove(&operation);
                            if let Some(reply) = reply {
                                let _ = reply.send(Ok(result));
                            }
                            Ok(())
                        }
                        Some($p::message::Kind::AcknowledgementConfirmation(value)) => {
                            let stream =
                                identity::decode_stream(value.stream.as_ref().ok_or_else(invalid)?)
                                    .map_err(|_| invalid())?;
                            if stream.client() != info.client || stream.pairing() != info.pairing {
                                return Err(denied());
                            }
                            self.tracker
                                .confirm_acknowledgement(stream, value.acknowledged_through)
                                .await
                        }
                        _ => Err(invalid()),
                    }
                };
            }
            match message {
                Payload::Major1(m) => receive!(m, v1),
                Payload::Major2(m) => receive!(m, v2),
            }
        })
    }
}
// A receive callback must use its writer channel. Dispatching through the
// registry here would wait for the reader that is running this callback.
struct OutcomePeer<'a> {
    channel: tokio::sync::Mutex<&'a mut dyn ProtocolChannel>,
    info: ConnectedInfo,
}
impl OperationPeer for OutcomePeer<'_> {
    fn validate_submission(
        &self,
        _: &PairedServerBinding,
        _: &Command,
        _: Option<spaceterm_core::identity::WorkspaceId>,
    ) -> Result<(), ClientFailure> {
        Err(denied())
    }
    fn submit<'a>(&'a self, _: &'a CanonicalOperationRequest) -> ClientFuture<'a, ()> {
        Box::pin(async { Err(denied()) })
    }
    fn lookup(&self, _: OperationBinding) -> ClientFuture<'_, OperationLookup> {
        Box::pin(async { Err(denied()) })
    }
    fn acknowledge(&self, stream: OperationStreamId, through: u64) -> ClientFuture<'_, ()> {
        Box::pin(async move {
            if stream.client() != self.info.client || stream.pairing() != self.info.pairing {
                return Err(denied());
            }
            let through = OperationSequence::new(through).map_err(|_| invalid())?;
            let message = match self.info.major {
                1 => Payload::Major1(v1::Message {
                    kind: Some(v1::message::Kind::Acknowledgement(
                        operations::v1::encode_acknowledgement(stream, through),
                    )),
                }),
                2 => Payload::Major2(v2::Message {
                    kind: Some(v2::message::Kind::Acknowledgement(
                        operations::v2::encode_acknowledgement(stream, through),
                    )),
                }),
                _ => return Err(invalid()),
            };
            self.channel.lock().await.send(&message).await
        })
    }
}

fn invalid() -> ClientFailure {
    ClientFailure::new(ClientFailureClass::InvalidPeer)
}
fn offline() -> ClientFailure {
    ClientFailure::new(ClientFailureClass::Unavailable)
}
fn denied() -> ClientFailure {
    ClientFailure::new(ClientFailureClass::AuthorizationDenied)
}

// The generic domain converter deliberately delegates identified pairing to its
// owner. This consumer installs only the ordinary self-unpair assessment grammar.
macro_rules! pairing_profile {
    ($p:ident, $module:ident) => {
        pub(super) mod $module {
            use super::*;
            use spaceterm_core::{authority::FieldVersion, pairing::*};
            use spaceterm_protocol::{pairing_adapter as pairing, wire::$p as wire};
            pub(crate) fn encode(
                request: &CanonicalOperationRequest,
            ) -> Result<wire::Operation, ClientFailure> {
                let Command::Pairing(PairingCommand::AssessUnpair { target }) = request.command()
                else {
                    return operations::$p::encode_request(request).map_err(|_| invalid());
                };
                Ok(wire::Operation {
                    id: Some(identity::encode_operation(request.binding().id)),
                    action: Some(wire::operation::Action::IdentifiedPairing(
                        wire::PairingRequest {
                            request: 0,
                            operation: Some(pairing::operation_to_wire(request.binding().id)),
                            action: Some(wire::pairing_request::Action::AssessUnpair(
                                spaceterm_protocol::wire::pairing::AssessUnpair {
                                    target: Some(pairing::target_to_wire(target)),
                                },
                            )),
                        },
                    )),
                })
            }
            pub(crate) fn decode(
                value: &wire::Operation,
                info: ConnectedInfo,
                codec: &dyn spaceterm_core::operation_ledger_contracts::LedgerCodec,
            ) -> Result<CanonicalOperationRequest, ClientFailure> {
                let Some(wire::operation::Action::IdentifiedPairing(pairing_request)) =
                    &value.action
                else {
                    return operations::$p::decode_request(value, info.pairing, info.client, codec)
                        .map_err(|_| invalid());
                };
                let id = identity::decode_operation(value.id.as_ref().ok_or_else(invalid)?)
                    .map_err(|_| invalid())?;
                if pairing_request.request != 0
                    || pairing::operation_from_wire(
                        pairing_request.operation.as_ref().ok_or_else(invalid)?,
                    )
                    .map_err(|_| invalid())?
                        != id
                    || id.stream().pairing() != info.pairing
                    || id.stream().client() != info.client
                {
                    return Err(invalid());
                }
                let Some(wire::pairing_request::Action::AssessUnpair(value)) =
                    &pairing_request.action
                else {
                    return Err(denied());
                };
                let target = pairing::target_from_wire(value.target.as_ref().ok_or_else(invalid)?)
                    .map_err(|_| invalid())?;
                CanonicalOperationRequest::new(
                    id,
                    Command::Pairing(PairingCommand::AssessUnpair { target }),
                    codec,
                )
                .map_err(|_| invalid())
            }
            pub(crate) fn outcome(
                value: &wire::Outcome,
                binding: OperationBinding,
            ) -> Result<(spaceterm_core::authority::OrganizationRevision, Outcome), ClientFailure>
            {
                let Some(wire::outcome::Result::PairingResult(result)) = &value.result else {
                    return operations::$p::decode_outcome(value, binding.id)
                        .map_err(|_| invalid());
                };
                if identity::decode_operation(value.operation.as_ref().ok_or_else(invalid)?)
                    .map_err(|_| invalid())?
                    != binding.id
                    || pairing::operation_from_wire(
                        result.operation.as_deref().ok_or_else(invalid)?,
                    )
                    .map_err(|_| invalid())?
                        != binding.id
                    || result.request != 0
                    || result.authorization_delivery.is_some()
                    || result.enrollment_receipt.is_some()
                    || result.client_binding_receipt.is_some()
                    || result.revocation_proof.is_some()
                    || result.reservation.is_some()
                {
                    return Err(invalid());
                }
                let revision = spaceterm_core::authority::OrganizationRevision::new(
                    value.publication_revision,
                );
                let value = result.unpair_assessment.as_deref().ok_or_else(invalid)?;
                if result.disposition != 4 || value.clients.len() > MAX_CLIENTS_PER_PAIRING {
                    return Err(invalid());
                }
                let target = pairing::target_from_wire(value.target.as_ref().ok_or_else(invalid)?)
                    .map_err(|_| invalid())?;
                let operation = pairing::operation_from_wire(
                    value.assessment_operation.as_ref().ok_or_else(invalid)?,
                )
                .map_err(|_| invalid())?;
                let digest = spaceterm_core::operations::RequestDigest::from_bytes(
                    value
                        .assessment_digest
                        .as_slice()
                        .try_into()
                        .map_err(|_| invalid())?,
                );
                if (operation, digest) != (binding.id, binding.digest) {
                    return Err(invalid());
                }
                let assessment = UnpairAssessment::new(UnpairAssessmentParts {
                    reference: spaceterm_core::operations::AssessmentRef { operation, digest },
                    target: target.clone(),
                    authorization_version: FieldVersion::new(value.authorization_version),
                    clients: value
                        .clients
                        .iter()
                        .map(|client| {
                            identity::decode_client(client).map(|id| ClientRecord {
                                id,
                                pairing: target.parts().pairing,
                            })
                        })
                        .collect::<Result<_, _>>()
                        .map_err(|_| invalid())?,
                })
                .map_err(|_| invalid())?;
                Ok((
                    revision,
                    Outcome::Pairing(Box::new(PairingOutcome::UnpairAssessed(assessment))),
                ))
            }
        }
    };
}

mod pairing {
    use super::*;
    pairing_profile!(v1, v1);
    pairing_profile!(v2, v2);
}
