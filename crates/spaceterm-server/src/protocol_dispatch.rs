//! Purpose-separated, bounded dispatch. Status subscriptions replace complete
//! bounded facts and never establish terminal-effect reconciliation.

mod terminal;

use crate::{
    adapters::transport::{protocol_failure, transport_failure},
    api::{AuthenticatedSession, ServerHandle, VerifiedPrincipal},
    connection::{AuthenticatedPeer, Authenticator, PeerConnection},
    contracts::*,
    egress::{ConnectionEgress, EgressBudget, EgressReservation},
    failure::failure,
    listeners::ConnectionDispatchPort,
};
use spaceterm_core::management::{ServerStatus, StatusPublication, StatusQuery};
use spaceterm_protocol::{
    ProtocolError,
    codec::Payload,
    convert::{management, organization},
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
                        $p::message::Kind::ClientContext(mut value) => {
                            if value.request == 0
                                || value.acknowledged
                                || value.failure.is_some()
                                || value.epoch != principal.epoch().get()
                                || value.connection_generation
                                    != principal.binding().generation.get()
                                || value.context_generation == 0
                            {
                                return Err(failure(ServerFailureClass::ScopeMismatch));
                            }
                            let update = crate::api::ClientFocusUpdate {
                                context: spaceterm_core::authority::ContextGeneration::new(
                                    value.context_generation,
                                ),
                                focused_pane: value
                                    .focused_pane
                                    .as_ref()
                                    .map(identity::decode_pane)
                                    .transpose()
                                    .map_err(protocol_failure)?,
                                terminal_input_focus: value.terminal_input_focus,
                                native_effects_eligible: value.native_effects_eligible,
                            };
                            match handle.update_client_focus(principal, update).await {
                                Ok(_) => value.acknowledged = true,
                                Err(error) if error.class == ServerFailureClass::StaleAuthority => {
                                    value.failure = Some(common::Failure {
                                        classification: common::FailureClass::Stale as i32,
                                        retry_after_ms: 0,
                                    })
                                }
                                Err(error) => return Err(error),
                            }
                            $p::message::Kind::ClientContext(value)
                        }
                        $p::message::Kind::RuntimeCommand(value) => {
                            let command =
                                spaceterm_protocol::convert::runtime::$p::decode_command(&value)
                                    .map_err(protocol_failure)?;
                            let fence = command.fence;
                            let result = handle.runtime_command(principal, command).await;
                            $p::message::Kind::RuntimeOutcome($p::RuntimeOutcome {
                                request: value.request,
                                fence: Some(identity::encode_fence(fence)),
                                failure: runtime_failure(result)?,
                            })
                        }
                        $p::message::Kind::ContentQuery(value) => {
                            let request =
                                spaceterm_protocol::convert::content::$p::decode_page_request(
                                    &value,
                                )
                                .map_err(protocol_failure)?;
                            let request = terminal::bounded_page_request(request)?;
                            let access = handle
                                .runtime_access(principal, request.stamp().fence().session())
                                .await?;
                            let result = access.handle.read_page(request)?.await?;
                            $p::message::Kind::ContentReadResponse(Box::new(
                                spaceterm_protocol::convert::content::$p::encode_page_result(
                                    &result,
                                )
                                .map_err(protocol_failure)?,
                            ))
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
    outcomes: Option<Arc<crate::listeners::outcomes::ConnectionOutcomes>>,
    account_runtime: Option<Arc<crate::account_runtime::AccountRuntimeProcesses>>,
    messages: Arc<Semaphore>,
    bytes: Arc<Semaphore>,
    egress: Arc<EgressBudget>,
    clock: Option<Arc<dyn crate::contracts::ServerClock>>,
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
            outcomes: None,
            account_runtime: None,
            messages: Arc::new(Semaphore::new(MAX_DECODED_DISPATCH_MESSAGES)),
            bytes: Arc::new(Semaphore::new(MAX_DECODED_DISPATCH_BYTES)),
            egress: EgressBudget::new(),
            clock: None,
        }
    }
    /// Installs the native lifecycle bridge on the paired local protocol route.
    pub fn with_account_runtime(
        mut self,
        runtime: Arc<crate::account_runtime::AccountRuntimeProcesses>,
    ) -> Self {
        self.account_runtime = Some(runtime);
        self
    }
    /// Shares the connection's reliable reply charge with actor admission.
    pub fn with_outcomes(
        mut self,
        outcomes: Arc<crate::listeners::outcomes::ConnectionOutcomes>,
    ) -> Self {
        self.outcomes = Some(outcomes);
        self
    }
    /// Share the runtime epoch clock for native-request deadline translation.
    pub fn with_clock(mut self, clock: Arc<dyn crate::contracts::ServerClock>) -> Self {
        self.clock = Some(clock);
        self
    }
    pub fn with_egress_budget(mut self, budget: Arc<EgressBudget>) -> Self {
        self.egress = budget;
        self
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
        let mut egress = ConnectionEgress::new(self.egress.clone());
        let mut terminal = terminal::TerminalConnection::new(self.clock.clone());
        let mut status = self.status.clone();
        let mut organization = handle.subscribe_snapshot();
        let mut organization_query: Option<organization::OrganizationQuery> = None;
        let mut awaiting_organization_resync = false;
        let mut organization_resyncs = 0u8;
        let mut subscription: Option<StatusQuery> = None;
        let mut sent: Option<(spaceterm_core::authority::ServerEpoch, NonZeroU64)> = None;
        loop {
            enum Wake {
                Message(Option<Vec<u8>>),
                Status,
                Organization,
                Terminal(Option<terminal::Wake>),
            }
            if *peer.closing.borrow() {
                return Ok(());
            }
            let wake = tokio::select! {
                _ = peer.closing.wait_for(|value| *value) => return Ok(()),
                event = terminal.next() => Wake::Terminal(event),
                result = peer.transport.recv() => Wake::Message(result.map_err(transport_failure)?),
                changed = status.changed(), if subscription.is_some() => { changed.map_err(|_| failure(ServerFailureClass::RuntimeUnavailable))?; Wake::Status },
                changed = organization.changed(), if organization_query.is_some() && !awaiting_organization_resync => { changed.map_err(|_| failure(ServerFailureClass::RuntimeUnavailable))?; Wake::Organization },
            };
            match wake {
                Wake::Terminal(event) => {
                    let PeerConnection::Ordinary(session) = &mut peer.connection else {
                        return Err(failure(ServerFailureClass::Authentication));
                    };
                    let event =
                        event.ok_or_else(|| failure(ServerFailureClass::RuntimeUnavailable))?;
                    match event {
                        terminal::Wake::Presentation(event) => {
                            terminal
                                .publish(
                                    *event,
                                    self,
                                    session,
                                    &peer.transport,
                                    &handle,
                                    &mut egress,
                                )
                                .await?
                        }
                        terminal::Wake::Effect(reply) => {
                            self.send(
                                session,
                                &peer.transport,
                                &reply.response?,
                                &mut egress,
                                Some(reply.reservation),
                            )
                            .await?
                        }
                    }
                }
                Wake::Organization => {
                    let query = organization_query
                        .ok_or_else(|| failure(ServerFailureClass::ScopeMismatch))?;
                    organization.borrow_and_update();
                    let PeerConnection::Ordinary(session) = &mut peer.connection else {
                        return Err(failure(ServerFailureClass::Authentication));
                    };
                    awaiting_organization_resync = !self
                        .send_organization(
                            session,
                            &peer.transport,
                            &handle,
                            query,
                            &mut egress,
                            &mut organization_resyncs,
                        )
                        .await?;
                }
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
                        self.send_status(session, &peer.transport, publication, &mut egress)
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
                        PeerConnection::Enrollment(codec) => {
                            let service = self
                                .pairing
                                .as_ref()
                                .ok_or_else(|| failure(ServerFailureClass::Authentication))?;
                            let response = service.enrollment_frame(codec, &bytes).await?;
                            peer.transport
                                .send(&response)
                                .await
                                .map_err(transport_failure)?;
                        }
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
                            if let Some(query) = organization_action(&payload)? {
                                if organization_query.is_some_and(|old| {
                                    (old.request == query.request
                                        && old.generation > query.generation)
                                        || (awaiting_organization_resync
                                            && query.generation <= old.generation)
                                }) {
                                    return Err(failure(ServerFailureClass::StaleAuthority));
                                }
                                organization.borrow_and_update();
                                awaiting_organization_resync = !self
                                    .send_organization(
                                        session,
                                        &peer.transport,
                                        &handle,
                                        query,
                                        &mut egress,
                                        &mut organization_resyncs,
                                    )
                                    .await?;
                                organization_query = Some(query);
                                continue;
                            }
                            if terminal
                                .subscription(&payload, session.principal, &handle)
                                .await?
                            {
                                continue;
                            }
                            if terminal.admit_effect(
                                &payload,
                                session.principal,
                                &handle,
                                &mut egress,
                            )? {
                                continue;
                            }
                            if let Some(runtime) = &self.account_runtime
                                && let Some(reply) = runtime
                                    .dispatch(&payload, peer.transport.peer_facts().local_uid)
                                    .await?
                            {
                                self.send(session, &peer.transport, &reply, &mut egress, None)
                                    .await?;
                                continue;
                            }
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
                                        self.send(
                                            session,
                                            &peer.transport,
                                            &payload,
                                            &mut egress,
                                            None,
                                        )
                                        .await?;
                                        subscription = None;
                                        sent = None;
                                    } else {
                                        let publication = StatusPublication::Snapshot {
                                            request: query.request,
                                            generation: query.generation,
                                            status: snapshot,
                                        };
                                        sent = Some(
                                            self.send_status(
                                                session,
                                                &peer.transport,
                                                publication,
                                                &mut egress,
                                            )
                                            .await?,
                                        );
                                        subscription = query.subscribe.then_some(query);
                                    }
                                }
                                None => {
                                    // Identified mutations share the four-request ceiling with pending runtime effects.
                                    let effectful = effectful_reply(&payload);
                                    if effectful && terminal.pending_effects() >= 4 {
                                        return Err(failure(ServerFailureClass::Capacity));
                                    }
                                    let reserved = if let Some(bytes) =
                                        terminal::page_reply_budget(&payload)?
                                    {
                                        Some(
                                            egress
                                                .reserve_content_reply(bytes)
                                                .map_err(egress_failure)?,
                                        )
                                    } else {
                                        effectful
                                            .then(|| egress.reserve_operation_reply())
                                            .transpose()
                                            .map_err(egress_failure)?
                                    };
                                    let scope = match (&self.outcomes, &reserved) {
                                        (Some(outcomes), Some(reserved)) if effectful => Some(
                                            outcomes.begin(session.principal, reserved.retain())?,
                                        ),
                                        _ => None,
                                    };
                                    if let Some(response) = self
                                        .ordinary
                                        .dispatch(session.principal, payload, handle.clone())
                                        .await?
                                    {
                                        if let Some(scope) = &scope {
                                            scope.validate_publication(&response)?;
                                        }
                                        if terminal::TerminalConnection::context_acknowledged(
                                            &response,
                                        ) {
                                            terminal.reset();
                                        }
                                        self.send(
                                            session,
                                            &peer.transport,
                                            &response,
                                            &mut egress,
                                            reserved,
                                        )
                                        .await?;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    async fn send_organization(
        &self,
        session: &mut AuthenticatedSession,
        transport: &Arc<dyn spaceterm_host::transport::MessageTransport>,
        handle: &ServerHandle,
        query: organization::OrganizationQuery,
        egress: &mut ConnectionEgress,
        resyncs: &mut u8,
    ) -> Result<bool, ServerFailure> {
        let mut transfer = handle
            .organization_snapshot(
                session.principal,
                session.codec.major(),
                query.request.get(),
                query.generation,
                // Both profiles need at most 128 framing bytes: fixed binding,
                // direction, purpose, major, a full u64 sequence, length prefixes
                // and Ed25519 signature under the 2 MiB envelope ceiling.
                session
                    .codec
                    .limits()
                    .management_bytes
                    .min(session.codec.limits().frame_bytes.saturating_sub(128)),
            )
            .await?;
        loop {
            let result = match transfer.next_page() {
                Ok(Some(page)) => self
                    .send_snapshot_page(session, transport, page, egress)
                    .await
                    .map(|_| false),
                Ok(None) => Ok(true),
                Err(error) => Err(error),
            };
            match result {
                Ok(true) => return Ok(true),
                Ok(false) => {}
                Err(error) if error.class == ServerFailureClass::StaleAuthority => {
                    // A partially staged client graph can only be replaced by a
                    // fresh request generation after an explicit reset marker.
                    if *resyncs != 0 {
                        return Err(failure(ServerFailureClass::Capacity));
                    }
                    *resyncs += 1;
                    let current = handle.snapshot();
                    let payload = match session.codec.major() {
                        1 => Payload::Major1(v1::Message {
                            kind: Some(v1::message::Kind::OrganizationResync(
                                v1::OrganizationResync {
                                    request: query.request.get(),
                                    request_generation: query.generation,
                                    epoch: current.epoch().get(),
                                    revision: current.state().revision.get(),
                                },
                            )),
                        }),
                        2 => Payload::Major2(v2::Message {
                            kind: Some(v2::message::Kind::OrganizationResync(
                                v2::OrganizationResync {
                                    request: query.request.get(),
                                    request_generation: query.generation,
                                    epoch: current.epoch().get(),
                                    revision: current.state().revision.get(),
                                },
                            )),
                        }),
                        _ => return Err(failure(ServerFailureClass::ScopeMismatch)),
                    };
                    self.send(session, transport, &payload, egress, None)
                        .await?;
                    return Ok(false);
                }
                Err(error) => return Err(error),
            }
        }
    }
    async fn send_snapshot_page(
        &self,
        session: &mut AuthenticatedSession,
        transport: &Arc<dyn spaceterm_host::transport::MessageTransport>,
        page: crate::publication::SnapshotPage,
        egress: &mut ConnectionEgress,
    ) -> Result<(), ServerFailure> {
        egress
            .enqueue(OutboundMessage::SnapshotPart(page), None)
            .map_err(egress_failure)?;
        self.authenticator.check_current(session.principal)?;
        let access = ConnectionAccessSnapshot {
            server: session.principal.server(),
            device: session.principal.device(),
            pairing: session.principal.pairing(),
            client: session.principal.client(),
            binding: session.principal.binding(),
            epoch: session.principal.epoch(),
            reconciled_revision: spaceterm_core::authority::OrganizationRevision::new(0),
            effects_enabled: false,
        };
        let pending = egress
            .take_next(&access)
            .ok_or_else(|| failure(ServerFailureClass::StaleAuthority))?;
        let id = pending.id();
        let result = async {
            let pending = pending.snapshot_payload()?;
            self.authenticator.check_current(session.principal)?;
            let bytes = session
                .codec
                .encode(pending.payload())
                .map_err(protocol_failure)?;
            self.write(transport, &bytes).await
        }
        .await;
        egress.complete_send(id);
        result
    }
    async fn send_status(
        &self,
        session: &mut AuthenticatedSession,
        transport: &Arc<dyn spaceterm_host::transport::MessageTransport>,
        publication: StatusPublication,
        egress: &mut ConnectionEgress,
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
        self.send(session, transport, &payload, egress, None)
            .await?;
        Ok(sent)
    }
    async fn send(
        &self,
        session: &mut AuthenticatedSession,
        transport: &Arc<dyn spaceterm_host::transport::MessageTransport>,
        payload: &Payload,
        egress: &mut ConnectionEgress,
        reserved: Option<EgressReservation>,
    ) -> Result<(), ServerFailure> {
        let _reservation = match reserved {
            Some(reserved) => {
                egress
                    .validate_reply_reservation(payload, &reserved)
                    .map_err(egress_failure)?;
                reserved
            }
            None => egress.reserve_reply(payload).map_err(egress_failure)?,
        };
        self.authenticator.check_current(session.principal)?;
        let bytes = session.codec.encode(payload).map_err(protocol_failure)?;
        self.write(transport, &bytes).await
    }
    async fn write(
        &self,
        transport: &Arc<dyn spaceterm_host::transport::MessageTransport>,
        bytes: &[u8],
    ) -> Result<(), ServerFailure> {
        let window = std::time::Duration::from_secs(5);
        let mut deadline = tokio::time::Instant::now() + window;
        let mut progress = transport.write_progress();
        let sending = transport.send(bytes);
        tokio::pin!(sending);
        loop {
            tokio::select! {
                biased;
                result = &mut sending => return result.map_err(transport_failure),
                changed = async {
                    match &mut progress {
                        Some(progress) => progress.changed().await,
                        None => std::future::pending().await,
                    }
                } => {
                    if changed.is_err() {
                        progress = None;
                    } else if let Some(progress) = &mut progress {
                        // Only successful native byte writes advance this clock.
                        // Buffered acceptance, wakeups and reads cannot extend it.
                        deadline = deadline.max(*progress.borrow_and_update() + window);
                    }
                }
                _ = tokio::time::sleep_until(deadline) => return Err(failure(ServerFailureClass::Capacity)),
            }
        }
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

fn organization_action(
    payload: &Payload,
) -> Result<Option<organization::OrganizationQuery>, ServerFailure> {
    match payload {
        Payload::Major1(v1::Message {
            kind: Some(v1::message::Kind::OrganizationQuery(query)),
        }) => organization::v1::decode_query(query)
            .map(Some)
            .map_err(protocol_failure),
        Payload::Major2(v2::Message {
            kind: Some(v2::message::Kind::OrganizationQuery(query)),
        }) => organization::v2::decode_query(query)
            .map(Some)
            .map_err(protocol_failure),
        _ => Ok(None),
    }
}

fn effectful_reply(payload: &Payload) -> bool {
    match payload {
        Payload::Major1(v1::Message {
            kind: Some(v1::message::Kind::Operation(_) | v1::message::Kind::RuntimeCommand(_)),
        })
        | Payload::Major2(v2::Message {
            kind: Some(v2::message::Kind::Operation(_) | v2::message::Kind::RuntimeCommand(_)),
        }) => true,
        Payload::Major1(v1::Message {
            kind: Some(v1::message::Kind::PairingRequest(request)),
        }) => request.operation.is_some(),
        Payload::Major2(v2::Message {
            kind: Some(v2::message::Kind::PairingRequest(request)),
        }) => request.operation.is_some(),
        _ => false,
    }
}
fn egress_failure(error: crate::egress::EgressFailure) -> ServerFailure {
    use crate::egress::EgressFailure;
    failure(match error {
        EgressFailure::Capacity => ServerFailureClass::Capacity,
        EgressFailure::ReservationMismatch => ServerFailureClass::ScopeMismatch,
        EgressFailure::Closed => ServerFailureClass::StaleAuthority,
        EgressFailure::Exhausted => ServerFailureClass::Exhausted,
    })
}

fn runtime_failure(
    result: Result<spaceterm_core::runtime_authority::EffectDisposition, ServerFailure>,
) -> Result<Option<common::Failure>, ServerFailure> {
    use spaceterm_core::runtime_authority::EffectDisposition;
    let class = match result {
        Ok(EffectDisposition::Applied) => return Ok(None),
        Ok(EffectDisposition::StaleAuthority | EffectDisposition::Cancelled) => {
            common::FailureClass::Stale
        }
        Ok(EffectDisposition::Retired) => common::FailureClass::Retired,
        Err(error) => match error.class {
            ServerFailureClass::StaleAuthority => common::FailureClass::Stale,
            ServerFailureClass::Capacity => common::FailureClass::Limit,
            ServerFailureClass::RuntimeUnavailable | ServerFailureClass::NativeUnavailable => {
                common::FailureClass::Unavailable
            }
            _ => return Err(error),
        },
    };
    Ok(Some(common::Failure {
        classification: class as i32,
        retry_after_ms: 0,
    }))
}
