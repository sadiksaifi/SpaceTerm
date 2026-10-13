//! Both-major publication and durable operation ports over the registry's existing actor.

mod organization;

use super::*;
use spaceterm_client::{
    api::{ConnectedInfo, ConnectionState, PairedServerBinding},
    operations::{OperationPeer, OperationStatus, OperationTicket},
    pane_channel::{
        EffectAdmission, EffectCorrelation, EffectPermit, FencedRuntimeCommand, PaneConnectionPort,
        PaneReadRequest, PaneReadSource, PaneUseReply, PaneUseRequest,
    },
    ports::{
        ConnectionDelegate, ConnectionInvalidation, OrganizationDecoder, OrganizationPublication,
        OutboundClass, ProtocolChannel,
    },
    registry::ServerHandle,
    replica::ReplicaSnapshot,
};
use spaceterm_core::{
    authority::*,
    control::*,
    geometry::{GridLimits, SUPPORTED_GRID_LIMITS},
    identity::*,
    operation_ledger_contracts::CanonicalOperationRequest,
    operations::*,
    organization::{ControlRecord, SessionPhase},
    pairing::LocalPairingKey,
};
use spaceterm_protocol::{
    codec::Payload,
    convert::{identity, operations, runtime},
    wire::{v1, v2},
};
use std::{
    collections::BTreeMap,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::sync::oneshot;

pub(crate) struct Decoder;
impl OrganizationDecoder for Decoder {
    fn decode(&self, payload: Payload) -> Result<OrganizationPublication, ClientFailure> {
        organization::decode(payload)
    }
}

struct QueuedEffect {
    permit: EffectPermit,
    bytes: usize,
}

pub(crate) struct NativeProtocol {
    journal: Arc<JournalController>,
    tracker: OperationTracker,
    registry: Mutex<Option<ClientRegistry>>,
    channels: Mutex<Option<Arc<PaneChannels>>>,
    infos: Mutex<BTreeMap<ServerId, ConnectedInfo>>,
    lookups: Mutex<BTreeMap<OperationId, oneshot::Sender<OperationLookup>>>,
    effects: Mutex<BTreeMap<(ServerId, u64), QueuedEffect>>,
    sequence: AtomicU64,
    serialized_use: tokio::sync::Mutex<()>,
    executor: tokio::runtime::Handle,
    owner: std::sync::OnceLock<std::sync::Weak<NativeProtocol>>,
}
impl NativeProtocol {
    pub(crate) fn new(journal: Arc<JournalController>, tracker: OperationTracker) -> Self {
        Self {
            journal,
            tracker,
            registry: Mutex::new(None),
            channels: Mutex::new(None),
            infos: Mutex::new(BTreeMap::new()),
            lookups: Mutex::new(BTreeMap::new()),
            effects: Mutex::new(BTreeMap::new()),
            sequence: AtomicU64::new(0),
            serialized_use: tokio::sync::Mutex::new(()),
            executor: tokio::runtime::Handle::current(),
            owner: std::sync::OnceLock::new(),
        }
    }
    pub(crate) fn set_channels(
        self: &Arc<Self>,
        channels: Arc<PaneChannels>,
    ) -> Result<(), ClientFailure> {
        self.owner
            .set(Arc::downgrade(self))
            .map_err(|_| invalid())?;
        *self.channels.lock().map_err(|_| invalid())? = Some(channels);
        Ok(())
    }
    fn channels(&self) -> Result<Arc<PaneChannels>, ClientFailure> {
        self.channels
            .lock()
            .map_err(|_| invalid())?
            .clone()
            .ok_or_else(stale)
    }
    fn registry(&self) -> Result<ClientRegistry, ClientFailure> {
        self.registry
            .lock()
            .map_err(|_| invalid())?
            .clone()
            .ok_or_else(stale)
    }
    fn handle(&self, server: ServerId) -> Result<ServerHandle, ClientFailure> {
        self.registry()?.server(server).ok_or_else(stale)
    }
    fn connected(&self, server: ServerId) -> Result<(ServerHandle, ConnectedInfo), ClientFailure> {
        let handle = self.handle(server)?;
        let info = match *handle.state().borrow() {
            ConnectionState::Connected { info } => info,
            _ => return Err(stale()),
        };
        Ok((handle, info))
    }
    fn binding(&self, server: ServerId) -> Result<PairedServerBinding, ClientFailure> {
        self.journal
            .snapshot()?
            .bindings()
            .iter()
            .find_map(|saved| {
                saved
                    .binding()
                    .ok()
                    .filter(|binding| binding.target().server() == server)
            })
            .ok_or_else(stale)
    }
    fn next(&self) -> Result<u64, ClientFailure> {
        self.sequence
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .map(|previous| previous + 1)
            .map_err(|_| fail(ClientFailureClass::CounterExhausted))
    }
    async fn completed(&self, ticket: OperationTicket) -> Result<Outcome, ClientFailure> {
        let mut state = ticket.status();
        let await_outcome = async {
            loop {
                let current = state.borrow().clone();
                match current {
                    OperationStatus::Completed { outcome, .. } => return Ok(outcome),
                    OperationStatus::Retired => return Err(stale()),
                    _ => state.changed().await.map_err(|_| stale())?,
                }
            }
        };
        tokio::time::timeout(std::time::Duration::from_secs(15), await_outcome)
            .await
            .map_err(|_| fail(ClientFailureClass::Unavailable))?
    }
    async fn submit_command(
        &self,
        workspace: WorkspaceId,
        command: Command,
    ) -> Result<OperationTicket, ClientFailure> {
        let binding = self.binding(workspace.server())?;
        self.tracker
            .submit(&binding, command, Some(workspace), self)
            .await
    }
    fn publish(
        &self,
        info: ConnectedInfo,
        snapshot: &ReplicaSnapshot,
    ) -> Result<(), ClientFailure> {
        let retained = self
            .handle(info.server)?
            .replica()
            .borrow()
            .retained()
            .cloned()
            .ok_or_else(stale)?;
        if retained.cursor != snapshot.cursor {
            return Err(stale());
        }
        self.infos
            .lock()
            .map_err(|_| invalid())?
            .insert(info.server, info);
        self.channels()?.publish(info, retained)
    }
}
impl NativeRegistryBinding for NativeProtocol {
    fn bind(&self, registry: ClientRegistry) -> Result<(), ClientFailure> {
        let mut retained = self.registry.lock().map_err(|_| invalid())?;
        if retained.is_some() {
            return Err(fail(ClientFailureClass::RecoveryRequired));
        }
        *retained = Some(registry);
        Ok(())
    }
}
impl OperationPeer for NativeProtocol {
    fn validate_submission(
        &self,
        binding: &PairedServerBinding,
        command: &Command,
        workspace: Option<WorkspaceId>,
    ) -> Result<(), ClientFailure> {
        if !self.journal.ordinary_use_allowed(binding)? {
            return Err(fail(ClientFailureClass::AuthorizationDenied));
        }
        let (_, info) = self.connected(binding.target().server())?;
        if info.client != binding.target().client() || info.pairing != binding.target().pairing() {
            return Err(stale());
        }
        if let Some(workspace) = workspace {
            if workspace.server() != info.server {
                return Err(invalid());
            }
            let snapshot = self
                .handle(info.server)?
                .replica()
                .borrow()
                .retained()
                .cloned()
                .ok_or_else(stale)?;
            if !snapshot
                .organization
                .state()
                .workspaces
                .iter()
                .any(|record| record.id == workspace)
            {
                return Err(fail(ClientFailureClass::TargetNotFound));
            }
        }
        let saved = self.journal.snapshot()?;
        let stream = saved
            .bindings()
            .iter()
            .find(|saved| saved.binding().is_ok_and(|saved| &saved == binding))
            .ok_or_else(stale)?
            .streams()
            .parts()
            .ordinary;
        command.validate_scope(stream).map_err(|_| invalid())
    }
    fn submit<'a>(&'a self, request: &'a CanonicalOperationRequest) -> ClientFuture<'a, ()> {
        Box::pin(async move {
            self.tracker.admit_dispatch(request.binding())?;
            let (handle, info) = self.connected(request.binding().id.stream().server())?;
            handle
                .dispatch(info, encode_operation(info.major, request)?)
                .await
        })
    }
    fn lookup(&self, binding: OperationBinding) -> ClientFuture<'_, OperationLookup> {
        Box::pin(async move {
            let (handle, info) = self.connected(binding.id.stream().server())?;
            let (send, receive) = oneshot::channel();
            {
                let mut pending = self.lookups.lock().map_err(|_| invalid())?;
                if pending.len() >= 64 || pending.contains_key(&binding.id) {
                    return Err(fail(ClientFailureClass::ResourceLimit));
                }
                pending.insert(binding.id, send);
            }
            let _cleanup = LookupCleanup {
                owner: self,
                id: binding.id,
            };
            handle
                .dispatch(info, encode_lookup(info.major, binding))
                .await?;
            tokio::time::timeout(std::time::Duration::from_secs(15), receive)
                .await
                .map_err(|_| fail(ClientFailureClass::Unavailable))?
                .map_err(|_| stale())
        })
    }
    fn acknowledge(&self, stream: OperationStreamId, through: u64) -> ClientFuture<'_, ()> {
        Box::pin(async move {
            let (handle, info) = self.connected(stream.server())?;
            handle
                .dispatch(info, encode_ack(info.major, stream, through)?)
                .await
        })
    }
}
struct LookupCleanup<'a> {
    owner: &'a NativeProtocol,
    id: OperationId,
}
impl Drop for LookupCleanup<'_> {
    fn drop(&mut self) {
        if let Ok(mut pending) = self.owner.lookups.lock() {
            pending.remove(&self.id);
        }
    }
}
impl ConnectionDelegate for NativeProtocol {
    fn invalidate(&self, binding: &PairedServerBinding, reason: ConnectionInvalidation) {
        let server = binding.target().server();
        if let Ok(channels) = self.channels() {
            let _ = channels.invalidate(server, reason);
        }
        if let Ok(mut effects) = self.effects.lock() {
            effects.retain(|(owner, _), _| *owner != server);
        }
        if let Ok(mut pending) = self.lookups.lock() {
            pending.retain(|id, _| id.stream().server() != server);
        }
        if let Ok(mut infos) = self.infos.lock() {
            infos.remove(&server);
        }
    }
    fn reconcile<'a>(
        &'a self,
        info: ConnectedInfo,
        replica: &'a ReplicaSnapshot,
        channel: &'a mut dyn ProtocolChannel,
    ) -> ClientFuture<'a, ()> {
        Box::pin(async move {
            self.infos
                .lock()
                .map_err(|_| invalid())?
                .insert(info.server, info);
            let binding = self.binding(info.server)?;
            let peer = DirectPeer {
                owner: self,
                info,
                channel: tokio::sync::Mutex::new(channel),
            };
            self.tracker.reconcile(&binding, &peer).await?;
            self.tracker.reconcile_grants(&binding, replica).await
        })
    }
    fn publication(
        &self,
        info: ConnectedInfo,
        replica: &ReplicaSnapshot,
    ) -> Result<(), ClientFailure> {
        self.publish(info, replica)
    }
    fn admit(
        &self,
        info: ConnectedInfo,
        message: &Payload,
    ) -> Result<OutboundClass, ClientFailure> {
        self.journal.ensure_healthy()?;
        let binding = self.binding(info.server)?;
        if !self.journal.ordinary_use_allowed(&binding)?
            || info.client != binding.target().client()
            || info.pairing != binding.target().pairing()
        {
            return Err(stale());
        }
        macro_rules! profile { ($message:expr, $p:ident) => {{
            match $message.kind.as_ref().ok_or_else(invalid)? {
                $p::message::Kind::Operation(value) => {
                    let request = operations::$p::decode_request(value, info.pairing, info.client, self.journal.codec()).map_err(ClientFailure::from)?;
                    self.tracker.admit_dispatch(request.binding())?; Ok(OutboundClass::Ordinary)
                }
                $p::message::Kind::OperationLookupRequest(value) => {
                    let requested = operations::$p::decode_lookup_request(value, info.pairing, info.client).map_err(ClientFailure::from)?;
                    if self.journal.snapshot()?.operation(requested.id).is_none_or(|record| record.binding != requested) { return Err(invalid()); }
                    Ok(OutboundClass::Lifecycle)
                }
                $p::message::Kind::Acknowledgement(value) => {
                    let (stream, through) = operations::$p::decode_acknowledgement(value, info.pairing, info.client).map_err(ClientFailure::from)?;
                    let state = self.journal.snapshot()?;
                    if state.stream(stream).is_none_or(|header| header.purpose != spaceterm_core::operation_ledger_contracts::StreamPurpose::Ordinary || through.get() > header.durable_prefix) { return Err(invalid()); }
                    Ok(OutboundClass::Lifecycle)
                }
                $p::message::Kind::RuntimeCommand(value) => {
                    let command = runtime::$p::decode_command(value).map_err(ClientFailure::from)?;
                    let effects = self.effects.lock().map_err(|_| invalid())?;
                    let permit = effects.get(&(info.server, value.request)).ok_or_else(stale)?;
                    if !permit.permit.is_current() || permit.permit.fence() != command.fence { return Err(stale()); }
                    Ok(OutboundClass::Ordinary)
                }
                _ => Err(fail(ClientFailureClass::MissingCapability)),
            }
        }}; }
        match message {
            Payload::Major1(message) => profile!(message, v1),
            Payload::Major2(message) => profile!(message, v2),
        }
    }
    fn receive<'a>(
        &'a self,
        info: ConnectedInfo,
        message: Payload,
        channel: &'a mut dyn ProtocolChannel,
    ) -> ClientFuture<'a, ()> {
        Box::pin(async move {
            let direct = DirectPeer {
                owner: self,
                info,
                channel: tokio::sync::Mutex::new(channel),
            };
            match decode_event(message, self.journal.as_ref(), info)? {
                Event::Operation(binding, lookup) => {
                    let pending = self
                        .lookups
                        .lock()
                        .map_err(|_| invalid())?
                        .remove(&binding.id);
                    if let Some(pending) = pending {
                        let _ = pending.send(lookup);
                    } else {
                        self.tracker.resolve_bound(binding, lookup, &direct).await?;
                    }
                }
                Event::Acknowledged(stream, through) => {
                    self.tracker
                        .confirm_acknowledgement(stream, through)
                        .await?
                }
                Event::Runtime(request, fence) => {
                    if let Some(permit) = self
                        .effects
                        .lock()
                        .map_err(|_| invalid())?
                        .remove(&(info.server, request))
                        && permit.permit.fence() != fence
                    {
                        return Err(invalid());
                    }
                }
            }
            Ok(())
        })
    }
}

struct DirectPeer<'a> {
    owner: &'a NativeProtocol,
    info: ConnectedInfo,
    channel: tokio::sync::Mutex<&'a mut dyn ProtocolChannel>,
}
impl OperationPeer for DirectPeer<'_> {
    fn validate_submission(
        &self,
        binding: &PairedServerBinding,
        command: &Command,
        workspace: Option<WorkspaceId>,
    ) -> Result<(), ClientFailure> {
        self.owner.validate_submission(binding, command, workspace)
    }
    fn submit<'a>(&'a self, request: &'a CanonicalOperationRequest) -> ClientFuture<'a, ()> {
        Box::pin(async move {
            self.owner.tracker.admit_dispatch(request.binding())?;
            self.channel
                .lock()
                .await
                .send(&encode_operation(self.info.major, request)?)
                .await
        })
    }
    fn lookup(&self, binding: OperationBinding) -> ClientFuture<'_, OperationLookup> {
        Box::pin(async move {
            let mut channel = self.channel.lock().await;
            channel
                .send(&encode_lookup(self.info.major, binding))
                .await?;
            for _ in 0..64 {
                let reply =
                    tokio::time::timeout(std::time::Duration::from_secs(15), channel.receive())
                        .await
                        .map_err(|_| fail(ClientFailureClass::Unavailable))??;
                match decode_event(reply, self.owner.journal.as_ref(), self.info)? {
                    Event::Operation(received, lookup) if received == binding => return Ok(lookup),
                    Event::Acknowledged(stream, through) => {
                        self.owner
                            .tracker
                            .confirm_acknowledgement(stream, through)
                            .await?
                    }
                    _ => return Err(stale()),
                }
            }
            Err(fail(ClientFailureClass::ResourceLimit))
        })
    }
    fn acknowledge(&self, stream: OperationStreamId, through: u64) -> ClientFuture<'_, ()> {
        Box::pin(async move {
            self.channel
                .lock()
                .await
                .send(&encode_ack(self.info.major, stream, through)?)
                .await
        })
    }
}

impl PaneConnectionPort for NativeProtocol {
    fn grid_limits(&self, info: ConnectedInfo) -> Result<GridLimits, ClientFailure> {
        let (_, current) = self.connected(info.server)?;
        if current.binding != info.binding {
            return Err(stale());
        }
        Ok(SUPPORTED_GRID_LIMITS)
    }
    fn explicit_use(&self, request: PaneUseRequest) -> ClientFuture<'_, PaneUseReply> {
        Box::pin(async move {
            let _serialized = self.serialized_use.lock().await;
            let (_, current) = self.connected(request.info.server)?;
            if current.binding != request.info.binding {
                return Err(stale());
            }
            let grant = match request.runtime.control {
                ControlRecord::Held { client, generation } if client == request.info.client => {
                    ControlGrant::new(request.runtime.pane, client, generation)
                        .map_err(|_| invalid())?
                }
                ControlRecord::Unclaimed => {
                    let ticket = self
                        .submit_command(
                            request.workspace,
                            Command::CompareAcquire(CompareAcquireRequest {
                                epoch: request.info.epoch,
                                pane: request.runtime.pane,
                                client: request.info.client,
                                expected_generation: request.runtime.control_generation,
                            }),
                        )
                        .await?;
                    match self.completed(ticket).await? {
                        Outcome::ControlGranted(grant) => grant.grant(),
                        Outcome::ControlAcquired(grant) => grant,
                        _ => return Err(stale()),
                    }
                }
                _ => return Err(stale()),
            };
            if matches!(
                request.runtime.phase,
                SessionPhase::Running { .. } | SessionPhase::Starting { .. }
            ) {
                return Ok(PaneUseReply::ControlReady(
                    GrantedControl::new(request.info.epoch, grant).map_err(|_| invalid())?,
                ));
            }
            let fence = EffectFence::new(
                SessionKey {
                    pane: request.runtime.pane,
                    epoch: request.info.epoch,
                    generation: request.runtime.session_generation,
                },
                grant,
                request.info.binding,
                request.context,
                None,
            )
            .map_err(|_| invalid())?;
            let binding = self.binding(request.info.server)?;
            let (intent, ticket) = self
                .tracker
                .begin_startup(&binding, fence, request.source, request.workspace, self)
                .await?;
            match self.completed(ticket).await? {
                Outcome::StartupAwaitingMeasurement { intent: received }
                    if received == intent.id() =>
                {
                    Ok(PaneUseReply::AwaitingMeasurement(intent))
                }
                _ => Err(stale()),
            }
        })
    }
    fn complete_intent(
        &self,
        workspace: WorkspaceId,
        measurement: MeasuredStartupMeasurement,
    ) -> ClientFuture<'_, OperationTicket> {
        Box::pin(async move {
            self.submit_command(workspace, Command::CompleteMeasuredStartup(measurement))
                .await
        })
    }
    fn cancel_intent(&self, intent: StartupIntent) {
        let Some(owner) = self.owner.get().and_then(std::sync::Weak::upgrade) else {
            return;
        };
        let workspace = self.journal.snapshot().ok().and_then(|saved| {
            saved
                .operation(intent.id())
                .and_then(|record| record.workspace)
        });
        let Some(workspace) = workspace else {
            return;
        };
        self.executor.spawn(async move {
            let _ = owner
                .submit_command(
                    workspace,
                    Command::CancelStartup {
                        intent: intent.id(),
                    },
                )
                .await;
        });
    }
    fn take_over(
        &self,
        workspace: WorkspaceId,
        request: TakeOverRequest,
    ) -> ClientFuture<'_, OperationTicket> {
        Box::pin(async move {
            self.submit_command(workspace, Command::TakeOver(request))
                .await
        })
    }
    fn try_effect(
        &self,
        permit: EffectPermit,
        command: FencedRuntimeCommand,
    ) -> Result<EffectAdmission, ClientFailure> {
        if !permit.is_current() || permit.fence() != command.fence {
            return Err(stale());
        }
        let server = command.fence.session().pane.server();
        let (handle, info) = self.connected(server)?;
        let request = self.next()?;
        let payload = match info.major {
            1 => Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::RuntimeCommand(
                    runtime::v1::encode_command(request, &command).map_err(ClientFailure::from)?,
                )),
            }),
            2 => Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::RuntimeCommand(
                    runtime::v2::encode_command(request, &command).map_err(ClientFailure::from)?,
                )),
            }),
            _ => return Err(invalid()),
        };
        let mut effects = self.effects.lock().map_err(|_| invalid())?;
        effects.retain(|_, queued| queued.permit.is_current());
        let bytes = payload.encoded_len();
        let resident = effects
            .values()
            .try_fold(0usize, |total, queued| total.checked_add(queued.bytes))
            .ok_or_else(|| fail(ClientFailureClass::ResourceLimit))?;
        if effects.len() >= 128
            || bytes > info.limits.frame_bytes
            || resident
                .checked_add(bytes)
                .is_none_or(|total| total > spaceterm_protocol::limits::MAX_EGRESS_BYTES)
        {
            return Err(fail(ClientFailureClass::ResourceLimit));
        }
        effects.insert((server, request), QueuedEffect { permit, bytes });
        drop(effects);
        let owner = self.owner.get().cloned().ok_or_else(stale)?;
        self.executor.spawn(async move {
            if handle.dispatch(info, payload).await.is_err()
                && let Some(owner) = owner.upgrade()
                && let Ok(mut effects) = owner.effects.lock()
            {
                effects.remove(&(server, request));
            }
        });
        Ok(EffectAdmission::Correlated(EffectCorrelation::new(
            request,
            command.fence,
        )?))
    }
    fn read(
        &self,
        _permit: EffectPermit,
        _request: PaneReadRequest,
    ) -> ClientFuture<'_, Box<dyn PaneReadSource>> {
        Box::pin(async { Err(fail(ClientFailureClass::MissingCapability)) })
    }
}
impl LocalUseInvalidator for NativeProtocol {
    fn invalidate_workspace(
        &self,
        _pairing: &LocalPairingKey,
        _workspace: WorkspaceId,
    ) -> Result<Vec<GrantedControl>, ClientFailure> {
        Err(stale())
    }
    fn invalidate_attachment(
        &self,
        pairing: &LocalPairingKey,
        attachment: spaceterm_client::view::WorkspaceAttachment,
    ) -> Result<Vec<GrantedControl>, ClientFailure> {
        self.channels()?.invalidate_attachment(pairing, attachment)
    }
    fn invalidate_pairing(&self, pairing: &LocalPairingKey) {
        if let Ok(binding) = self.binding(pairing.parts().server)
            && binding
                .target()
                .local_pairing()
                .is_ok_and(|key| &key == pairing)
        {
            self.invalidate(&binding, ConnectionInvalidation::BindingRemoved);
        }
    }
    fn remove_pairing<'a>(&'a self, pairing: &'a LocalPairingKey) -> ClientFuture<'a, ()> {
        Box::pin(async move {
            self.registry()?
                .remove_suppressed(pairing)
                .await
                .map(|_| ())
        })
    }
}

fn encode_operation(
    major: u32,
    request: &CanonicalOperationRequest,
) -> Result<Payload, ClientFailure> {
    match major {
        1 => Ok(Payload::Major1(v1::Message {
            kind: Some(v1::message::Kind::Operation(
                operations::v1::encode_request(request).map_err(ClientFailure::from)?,
            )),
        })),
        2 => Ok(Payload::Major2(v2::Message {
            kind: Some(v2::message::Kind::Operation(
                operations::v2::encode_request(request).map_err(ClientFailure::from)?,
            )),
        })),
        _ => Err(invalid()),
    }
}
fn encode_lookup(major: u32, binding: OperationBinding) -> Payload {
    if major == 1 {
        Payload::Major1(v1::Message {
            kind: Some(v1::message::Kind::OperationLookupRequest(
                operations::v1::encode_lookup_request(binding),
            )),
        })
    } else {
        Payload::Major2(v2::Message {
            kind: Some(v2::message::Kind::OperationLookupRequest(
                operations::v2::encode_lookup_request(binding),
            )),
        })
    }
}
fn encode_ack(
    major: u32,
    stream: OperationStreamId,
    through: u64,
) -> Result<Payload, ClientFailure> {
    let through = OperationSequence::new(through).map_err(|_| invalid())?;
    Ok(if major == 1 {
        Payload::Major1(v1::Message {
            kind: Some(v1::message::Kind::Acknowledgement(
                operations::v1::encode_acknowledgement(stream, through),
            )),
        })
    } else {
        Payload::Major2(v2::Message {
            kind: Some(v2::message::Kind::Acknowledgement(
                operations::v2::encode_acknowledgement(stream, through),
            )),
        })
    })
}
enum Event {
    Operation(OperationBinding, OperationLookup),
    Acknowledged(OperationStreamId, u64),
    Runtime(u64, EffectFence),
}
fn decode_event(
    message: Payload,
    journal: &JournalController,
    info: ConnectedInfo,
) -> Result<Event, ClientFailure> {
    macro_rules! profile {
        ($message:expr, $p:ident) => {{
            match $message.kind.ok_or_else(invalid)? {
                $p::message::Kind::Outcome(value) => {
                    let id =
                        identity::decode_operation(value.operation.as_ref().ok_or_else(invalid)?)
                            .map_err(ClientFailure::from)?;
                    let saved = journal.snapshot()?;
                    let binding = saved.operation(id).ok_or_else(invalid)?.binding;
                    if id.stream().client() != info.client || id.stream().pairing() != info.pairing
                    {
                        return Err(invalid());
                    }
                    let (publication_revision, result) =
                        operations::$p::decode_outcome(&value, id).map_err(ClientFailure::from)?;
                    Ok(Event::Operation(
                        binding,
                        OperationLookup::Completed {
                            result,
                            publication_revision,
                        },
                    ))
                }
                $p::message::Kind::OperationLookupResult(value) => {
                    let id =
                        identity::decode_operation(value.operation.as_ref().ok_or_else(invalid)?)
                            .map_err(ClientFailure::from)?;
                    let saved = journal.snapshot()?;
                    let binding = saved.operation(id).ok_or_else(invalid)?.binding;
                    if id.stream().client() != info.client || id.stream().pairing() != info.pairing
                    {
                        return Err(invalid());
                    }
                    Ok(Event::Operation(
                        binding,
                        operations::$p::decode_lookup_result(&value, binding)
                            .map_err(ClientFailure::from)?,
                    ))
                }
                $p::message::Kind::AcknowledgementConfirmation(value) => {
                    let stream =
                        identity::decode_stream(value.stream.as_ref().ok_or_else(invalid)?)
                            .map_err(ClientFailure::from)?;
                    if stream.client() != info.client || stream.pairing() != info.pairing {
                        return Err(invalid());
                    }
                    Ok(Event::Acknowledged(stream, value.acknowledged_through))
                }
                $p::message::Kind::RuntimeOutcome(value) => Ok(Event::Runtime(
                    value.request,
                    identity::decode_fence(value.fence.as_ref().ok_or_else(invalid)?, None)
                        .map_err(ClientFailure::from)?,
                )),
                _ => Err(fail(ClientFailureClass::MissingCapability)),
            }
        }};
    }
    match message {
        Payload::Major1(message) => profile!(message, v1),
        Payload::Major2(message) => profile!(message, v2),
    }
}
fn invalid() -> ClientFailure {
    fail(ClientFailureClass::InvalidPeer)
}
fn stale() -> ClientFailure {
    fail(ClientFailureClass::StaleAuthority)
}

#[cfg(test)]
mod tests;
