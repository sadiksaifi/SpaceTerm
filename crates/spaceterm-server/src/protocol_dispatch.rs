//! Purpose-separated, bounded dispatch. Status subscriptions replace complete
//! bounded facts and never establish terminal-effect reconciliation.

use crate::{
    adapters::transport::{protocol_failure, transport_failure},
    api::{AuthenticatedSession, ServerHandle, VerifiedPrincipal},
    connection::{AuthenticatedPeer, Authenticator, PeerConnection},
    contracts::*,
    failure::failure,
    listeners::ConnectionDispatchPort,
};
use spaceterm_core::management::{ServerStatus, StatusPublication, StatusQuery};
use spaceterm_protocol::{
    ProtocolError,
    codec::Payload,
    convert::management,
    wire::{common, v1, v2},
};
use std::{num::NonZeroU64, sync::Arc};
use tokio::sync::{Semaphore, watch};

pub const MAX_DECODED_DISPATCH_MESSAGES: usize = 256;
pub const MAX_DECODED_DISPATCH_BYTES: usize = 16 * 1024 * 1024;

/// The ordinary adapter consumes only an authenticated principal. Identified
/// operations must use actor ledger admission, never an ephemeral request ID.
/// Publication and terminal resource replies retain their owning egress policy.
pub trait OrdinaryDispatchPort: Send + Sync {
    fn dispatch(
        &self,
        principal: VerifiedPrincipal,
        payload: Payload,
        handle: ServerHandle,
    ) -> ServerFuture<'_, Result<Option<Payload>, ServerFailure>>;
}
/// Ordinary identified management requests share the actor's canonical codec.
/// This adapter holds no reducer, ledger, store or runtime ownership.
pub struct ActorOrdinaryDispatch {
    codec: Arc<dyn spaceterm_core::operation_ledger_contracts::LedgerCodec>,
}
impl ActorOrdinaryDispatch {
    pub fn new(codec: Arc<dyn spaceterm_core::operation_ledger_contracts::LedgerCodec>) -> Self {
        Self { codec }
    }
}
impl OrdinaryDispatchPort for ActorOrdinaryDispatch {
    fn dispatch(
        &self,
        principal: VerifiedPrincipal,
        payload: Payload,
        handle: ServerHandle,
    ) -> ServerFuture<'_, Result<Option<Payload>, ServerFailure>> {
        Box::pin(async move {
            use crate::api::OperationResponse;
            use spaceterm_core::{authority::OrganizationRevision, operations::*};
            use spaceterm_protocol::convert::{identity, operations};
            macro_rules! profile {
                ($message:expr, $p:ident, $major:ident) => {{
                    let response = match $message
                        .kind
                        .ok_or_else(|| failure(ServerFailureClass::ScopeMismatch))?
                    {
                        $p::message::Kind::Operation(value) => {
                            let request = operations::$p::decode_request(
                                &value,
                                principal.pairing(),
                                principal.client(),
                                self.codec.as_ref(),
                            )
                            .map_err(protocol_failure)?;
                            let binding = request.binding();
                            match handle.submit(principal, request).await? {
                                OperationResponse::Completed {
                                    binding: completed,
                                    revision,
                                    outcome,
                                } => {
                                    if completed != binding {
                                        return Err(failure(ServerFailureClass::RecoveryRequired));
                                    }
                                    $p::message::Kind::Outcome(
                                        operations::$p::encode_outcome(
                                            binding.id, revision, &outcome,
                                        )
                                        .map_err(protocol_failure)?,
                                    )
                                }
                                OperationResponse::Pending(pending) => {
                                    if pending != binding {
                                        return Err(failure(ServerFailureClass::RecoveryRequired));
                                    }
                                    $p::message::Kind::OperationLookupResult(
                                        operations::$p::encode_lookup_result(
                                            binding,
                                            &OperationLookup::Pending,
                                        )
                                        .map_err(protocol_failure)?,
                                    )
                                }
                                OperationResponse::Retired => {
                                    $p::message::Kind::OperationLookupResult(
                                        operations::$p::encode_lookup_result(
                                            binding,
                                            &OperationLookup::AcknowledgedRetired,
                                        )
                                        .map_err(protocol_failure)?,
                                    )
                                }
                            }
                        }
                        $p::message::Kind::OperationLookupRequest(value) => {
                            let binding = operations::$p::decode_lookup_request(
                                &value,
                                principal.pairing(),
                                principal.client(),
                            )
                            .map_err(protocol_failure)?;
                            let result = handle.lookup_state(principal, binding).await?;
                            $p::message::Kind::OperationLookupResult(
                                operations::$p::encode_lookup_result(binding, &result)
                                    .map_err(protocol_failure)?,
                            )
                        }
                        $p::message::Kind::Acknowledgement(value) => {
                            let (stream, through) = operations::$p::decode_acknowledgement(
                                &value,
                                principal.pairing(),
                                principal.client(),
                            )
                            .map_err(protocol_failure)?;
                            handle.acknowledge(principal, stream, through).await?;
                            $p::message::Kind::AcknowledgementConfirmation(
                                $p::AcknowledgementConfirmation {
                                    stream: Some(identity::encode_stream(stream)),
                                    acknowledged_through: through.get(),
                                },
                            )
                        }
                        $p::message::Kind::Reconciliation(mut value) => {
                            if value.request == 0
                                || value.acknowledged
                                || value.connection_generation != 0
                                || value.grid_limits.is_some()
                                || identity::decode_server(
                                    value.server.as_ref().ok_or_else(|| {
                                        failure(ServerFailureClass::ScopeMismatch)
                                    })?,
                                )
                                .map_err(protocol_failure)?
                                    != principal.server()
                                || identity::decode_client(
                                    value.client.as_ref().ok_or_else(|| {
                                        failure(ServerFailureClass::ScopeMismatch)
                                    })?,
                                )
                                .map_err(protocol_failure)?
                                    != principal.client()
                                || value.epoch != principal.epoch().get()
                            {
                                return Err(failure(ServerFailureClass::ScopeMismatch));
                            }
                            let access = handle
                                .reconcile(
                                    principal,
                                    OrganizationRevision::new(value.organization_revision),
                                )
                                .await?;
                            value.connection_generation = access.binding.generation.get();
                            value.acknowledged = true;
                            $p::message::Kind::Reconciliation(value)
                        }
                        // Standalone control/startup requests have no durable
                        // operation identity. They cannot reach actor admission.
                        _ => return Err(failure(ServerFailureClass::ScopeMismatch)),
                    };
                    Ok(Some(Payload::$major($p::Message {
                        kind: Some(response),
                    })))
                }};
            }
            match payload {
                Payload::Major1(message) => profile!(message, v1, Major1),
                Payload::Major2(message) => profile!(message, v2, Major2),
            }
        })
    }
}

/// Pairing owns this restricted policy. It receives no ServerHandle and cannot
/// acquire an ordinary principal or dispatch terminal work through this port.
pub trait RestrictedDispatchPort: Send + Sync {
    fn dispatch(
        &self,
        principal: spaceterm_protocol::authentication::Principal,
        payload: common::RestrictedMessage,
    ) -> ServerFuture<'_, Result<common::RestrictedMessage, ServerFailure>>;
}

pub struct ProtocolDispatcher {
    authenticator: Authenticator,
    status: watch::Receiver<ServerStatus>,
    ordinary: Arc<dyn OrdinaryDispatchPort>,
    restricted: Arc<dyn RestrictedDispatchPort>,
    pairing: Option<crate::pairing::service::PairingService>,
    messages: Arc<Semaphore>,
    bytes: Arc<Semaphore>,
}
impl ProtocolDispatcher {
    /// The native/actor status producer supplies this validated bounded feed.
    /// NativeVerification remains part of every publication, not implied by IPC.
    pub fn new(
        authenticator: Authenticator,
        status: watch::Receiver<ServerStatus>,
        ordinary: Arc<dyn OrdinaryDispatchPort>,
        restricted: Arc<dyn RestrictedDispatchPort>,
    ) -> Self {
        Self {
            authenticator,
            status,
            ordinary,
            restricted,
            pairing: None,
            messages: Arc::new(Semaphore::new(MAX_DECODED_DISPATCH_MESSAGES)),
            bytes: Arc::new(Semaphore::new(MAX_DECODED_DISPATCH_BYTES)),
        }
    }
    /// Select pairing's additive restricted grammar before receiving any frame.
    /// A failed decode never falls back to another grammar or ordinary dispatch.
    pub fn with_pairing(mut self, service: crate::pairing::service::PairingService) -> Self {
        self.pairing = Some(service);
        self
    }
    async fn dispatch_restricted(
        &self,
        codec: &mut spaceterm_protocol::connection::RestrictedConnection,
        bytes: &[u8],
    ) -> Result<Vec<u8>, ServerFailure> {
        if let Some(service) = &self.pairing {
            crate::pairing::restricted::dispatch_frame(service, codec, bytes).await
        } else {
            let payload = codec.decode(bytes).map_err(protocol_failure)?;
            let response = self.restricted.dispatch(codec.principal(), payload).await?;
            codec.encode(&response).map_err(protocol_failure)
        }
    }
    async fn run_peer(
        &self,
        peer: &mut AuthenticatedPeer,
        handle: ServerHandle,
    ) -> Result<(), ServerFailure> {
        let mut status = self.status.clone();
        let mut subscription: Option<StatusQuery> = None;
        let mut sent: Option<(spaceterm_core::authority::ServerEpoch, NonZeroU64)> = None;
        loop {
            enum Wake {
                Message(Option<Vec<u8>>),
                Status,
            }
            let wake = tokio::select! {
                biased;
                _ = peer.closing.wait_for(|value| *value) => return Ok(()),
                result = peer.transport.recv() => Wake::Message(result.map_err(transport_failure)?),
                changed = status.changed(), if subscription.is_some() => { changed.map_err(|_| failure(ServerFailureClass::RuntimeUnavailable))?; Wake::Status },
            };
            match wake {
                Wake::Status => {
                    let query =
                        subscription.ok_or_else(|| failure(ServerFailureClass::StaleAuthority))?;
                    let snapshot = status.borrow_and_update().clone();
                    let publication = match sent {
                        Some((epoch, base))
                            if epoch == snapshot.parts().epoch
                                && base.get().checked_add(1)
                                    == Some(snapshot.parts().revision.get()) =>
                        {
                            StatusPublication::Changed {
                                request: query.request,
                                generation: query.generation,
                                base,
                                status: snapshot,
                            }
                        }
                        _ => StatusPublication::Snapshot {
                            request: query.request,
                            generation: query.generation,
                            status: snapshot,
                        },
                    };
                    let PeerConnection::Ordinary(session) = &mut peer.connection else {
                        return Err(failure(ServerFailureClass::Authentication));
                    };
                    sent = Some(
                        self.send_status(session, &peer.transport, publication)
                            .await?,
                    );
                }
                Wake::Message(None) => return Ok(()),
                Wake::Message(Some(bytes)) => {
                    let _message = self
                        .messages
                        .clone()
                        .try_acquire_owned()
                        .map_err(|_| failure(ServerFailureClass::Capacity))?;
                    // Charge retained wire bytes and a conservative decoded-object
                    // allowance. Overflow fails before generated DTO construction.
                    let charge = bytes
                        .len()
                        .checked_mul(8)
                        .and_then(|v| u32::try_from(v).ok())
                        .ok_or_else(|| failure(ServerFailureClass::Capacity))?;
                    let _bytes = self
                        .bytes
                        .clone()
                        .try_acquire_many_owned(charge)
                        .map_err(|_| failure(ServerFailureClass::Capacity))?;
                    match &mut peer.connection {
                        PeerConnection::Restricted(codec) => {
                            let bytes = self.dispatch_restricted(codec, &bytes).await?;
                            peer.transport
                                .send(&bytes)
                                .await
                                .map_err(transport_failure)?;
                        }
                        PeerConnection::Ordinary(session) => {
                            self.authenticator.check_current(session.principal)?;
                            let payload = session.codec.decode(&bytes).map_err(protocol_failure)?;
                            let action = status_action(&payload, session.principal)?;
                            match action {
                                Some(StatusAction::Cancel {
                                    request,
                                    generation,
                                }) => {
                                    if subscription.is_some_and(|query| {
                                        query.request == request && query.generation == generation
                                    }) {
                                        subscription = None;
                                        sent = None;
                                    }
                                }
                                Some(StatusAction::Query(query, legacy)) => {
                                    if subscription.is_some_and(|old| {
                                        old.request == query.request
                                            && old.generation > query.generation
                                    }) {
                                        return Err(failure(ServerFailureClass::StaleAuthority));
                                    }
                                    let snapshot = status.borrow_and_update().clone();
                                    if legacy {
                                        validate_status(session.principal, &snapshot)?;
                                        let payload = legacy_status(
                                            session.codec.major(),
                                            query.request.get(),
                                            &snapshot,
                                        )
                                        .map_err(protocol_failure)?;
                                        self.send(session, &peer.transport, &payload).await?;
                                        subscription = None;
                                        sent = None;
                                    } else {
                                        let publication = StatusPublication::Snapshot {
                                            request: query.request,
                                            generation: query.generation,
                                            status: snapshot,
                                        };
                                        sent = Some(
                                            self.send_status(session, &peer.transport, publication)
                                                .await?,
                                        );
                                        subscription = query.subscribe.then_some(query);
                                    }
                                }
                                None => {
                                    // One sequential request is stricter than the
                                    // four-effectful-request per-connection ceiling.
                                    if let Some(response) = self
                                        .ordinary
                                        .dispatch(session.principal, payload, handle.clone())
                                        .await?
                                    {
                                        self.send(session, &peer.transport, &response).await?;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    async fn send_status(
        &self,
        session: &mut AuthenticatedSession,
        transport: &Arc<dyn spaceterm_host::transport::MessageTransport>,
        publication: StatusPublication,
    ) -> Result<(spaceterm_core::authority::ServerEpoch, NonZeroU64), ServerFailure> {
        let snapshot = match &publication {
            StatusPublication::Snapshot { status, .. }
            | StatusPublication::Changed { status, .. } => status,
            _ => return Err(failure(ServerFailureClass::ScopeMismatch)),
        };
        validate_status(session.principal, snapshot)?;
        let sent = (snapshot.parts().epoch, snapshot.parts().revision);
        let payload = match session.codec.major() {
            1 => Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::StatusPublication(
                    management::v1::encode_publication(&publication),
                )),
            }),
            2 => Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::StatusPublication(
                    management::v2::encode_publication(&publication),
                )),
            }),
            _ => return Err(failure(ServerFailureClass::ScopeMismatch)),
        };
        self.send(session, transport, &payload).await?;
        Ok(sent)
    }
    async fn send(
        &self,
        session: &mut AuthenticatedSession,
        transport: &Arc<dyn spaceterm_host::transport::MessageTransport>,
        payload: &Payload,
    ) -> Result<(), ServerFailure> {
        self.authenticator.check_current(session.principal)?;
        let bytes = session.codec.encode(payload).map_err(protocol_failure)?;
        tokio::time::timeout(std::time::Duration::from_secs(5), transport.send(&bytes))
            .await
            .map_err(|_| failure(ServerFailureClass::Capacity))?
            .map_err(transport_failure)
    }
}
impl ConnectionDispatchPort for ProtocolDispatcher {
    fn run<'a>(
        &'a self,
        peer: &'a mut AuthenticatedPeer,
        handle: ServerHandle,
    ) -> ServerFuture<'a, Result<(), ServerFailure>> {
        Box::pin(self.run_peer(peer, handle))
    }
}

fn validate_status(
    principal: VerifiedPrincipal,
    status: &ServerStatus,
) -> Result<(), ServerFailure> {
    if status.parts().server != principal.server() || status.parts().epoch != principal.epoch() {
        return Err(failure(ServerFailureClass::ScopeMismatch));
    }
    Ok(())
}
enum StatusAction {
    Query(StatusQuery, bool),
    Cancel {
        request: NonZeroU64,
        generation: u64,
    },
}
fn status_action(
    payload: &Payload,
    principal: VerifiedPrincipal,
) -> Result<Option<StatusAction>, ServerFailure> {
    macro_rules! profile {
        ($message:expr,$p:ident) => {{
            match &$message.kind {
                Some($p::message::Kind::StatusQuery(query)) => {
                    if query.cancel {
                        if query.subscribe || query.known_epoch != 0 || query.known_revision != 0 {
                            return Err(failure(ServerFailureClass::ScopeMismatch));
                        }
                        Ok(Some(StatusAction::Cancel {
                            request: NonZeroU64::new(query.request)
                                .ok_or_else(|| failure(ServerFailureClass::ScopeMismatch))?,
                            generation: query.request_generation,
                        }))
                    } else {
                        Ok(Some(StatusAction::Query(
                            management::$p::decode_query(query, principal.binding())
                                .map_err(protocol_failure)?,
                            false,
                        )))
                    }
                }
                Some($p::message::Kind::ManagementRequest(request)) => {
                    if request.action != 1 {
                        return Ok(None);
                    }
                    if !request.authorization.is_empty()
                        || !request.assessment.is_empty()
                        || !request.version.is_empty()
                    {
                        return Err(failure(ServerFailureClass::ScopeMismatch));
                    }
                    Ok(Some(StatusAction::Query(
                        StatusQuery {
                            connection: principal.binding(),
                            request: NonZeroU64::new(request.request)
                                .ok_or_else(|| failure(ServerFailureClass::ScopeMismatch))?,
                            generation: 0,
                            known_epoch: None,
                            known_revision: None,
                            subscribe: false,
                        },
                        true,
                    )))
                }
                _ => Ok(None),
            }
        }};
    }
    match payload {
        Payload::Major1(message) => profile!(message, v1),
        Payload::Major2(message) => profile!(message, v2),
    }
}
fn legacy_status(
    major: u32,
    request: u64,
    status: &ServerStatus,
) -> Result<Payload, ProtocolError> {
    match major {
        1 => {
            let mut value = management::v1::encode_status(status);
            value.request = request;
            Ok(Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::ManagementStatus(value)),
            }))
        }
        2 => {
            let mut value = management::v2::encode_status(status);
            value.request = request;
            Ok(Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::ManagementStatus(value)),
            }))
        }
        _ => Err(ProtocolError::Incompatible),
    }
}
