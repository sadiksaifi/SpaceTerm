//! Connection-owned presentation demand. Dropping demand never retires a Session.

mod graphics;

use super::*;
use crate::egress::EgressFailure;
use spaceterm_core::{authority::*, content::ContentBase, content_queries::*, identity::PaneId};
use spaceterm_protocol::convert::{content, identity, native_requests};
use std::collections::BTreeMap;
use tokio::{sync::mpsc, task::AbortHandle};

const MAX_SUBSCRIPTIONS: usize = 64;
const MAX_PENDING_NATIVE: usize = 128;
const PAGE_BYTES: u32 = 128 * 1024;
#[derive(Clone, Copy, Eq, PartialEq)]
struct Token {
    request: u64,
    generation: u64,
    fence: EffectFence,
}
struct Subscription {
    token: Token,
    limits: PageLimits,
    transfer: u64,
    base: Option<ContentBase>,
    task: AbortHandle,
}
pub(super) struct Event {
    token: Token,
    event: Option<Result<RuntimePresentationEvent, ServerFailure>>,
}
pub(super) struct EffectReply {
    pub(super) response: Result<Payload, ServerFailure>,
    pub(super) reservation: EgressReservation,
    id: u64,
}
pub(super) enum Wake {
    Presentation(Box<Event>),
    Effect(Box<EffectReply>),
    Graphics(Box<graphics::GraphicsEvent>),
}
pub(super) struct TerminalConnection {
    graphics: graphics::GraphicsConnection,
    subscriptions: BTreeMap<PaneId, Subscription>,
    native: BTreeMap<(SessionKey, u64), spaceterm_core::native_requests::NativeRequest>,
    sender: mpsc::Sender<Event>,
    receiver: mpsc::Receiver<Event>,
    clock: Option<Arc<dyn crate::contracts::ServerClock>>,
    last_effect_request: u64,
    effect_sequence: u64,
    effects: BTreeMap<u64, AbortHandle>,
    effect_sender: mpsc::Sender<EffectReply>,
    effect_receiver: mpsc::Receiver<EffectReply>,
}
impl Drop for TerminalConnection {
    fn drop(&mut self) {
        self.reset();
        for task in self.effects.values() {
            task.abort();
        }
    }
}
impl TerminalConnection {
    pub(super) fn new(
        clock: Option<Arc<dyn crate::contracts::ServerClock>>,
        limits: spaceterm_protocol::limits::NegotiatedLimits,
    ) -> Self {
        let (sender, receiver) = mpsc::channel(16);
        let (effect_sender, effect_receiver) = mpsc::channel(4);
        Self {
            graphics: graphics::GraphicsConnection::new(limits),
            effect_sender,
            effect_receiver,
            effect_sequence: 0,
            effects: BTreeMap::new(),
            subscriptions: BTreeMap::new(),
            native: BTreeMap::new(),
            sender,
            receiver,
            clock,
            last_effect_request: 0,
        }
    }
    pub(super) fn pending_effects(&self) -> usize {
        self.effects.len()
    }
    pub(super) fn reset(&mut self) {
        self.graphics.reset();
        for (_, subscription) in std::mem::take(&mut self.subscriptions) {
            subscription.task.abort();
        }
        self.native.clear();
    }
    pub(super) async fn next(&mut self) -> Option<Wake> {
        tokio::select! {
            event = self.graphics.next() => event.map(|event| Wake::Graphics(Box::new(event))),
            event = self.receiver.recv() => event.map(|event| Wake::Presentation(Box::new(event))),
            reply = self.effect_receiver.recv() => reply.map(|reply| { self.effects.remove(&reply.id); Wake::Effect(Box::new(reply)) }),
        }
    }
    pub(super) fn effect_request(&mut self, payload: &Payload) -> Result<(), ServerFailure> {
        let request = match payload {
            Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::RuntimeCommand(value)),
            }) => value.request,
            Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::RuntimeCommand(value)),
            }) => value.request,
            _ => return Ok(()),
        };
        if request <= self.last_effect_request {
            return Err(failure(ServerFailureClass::StaleAuthority));
        }
        self.last_effect_request = request;
        Ok(())
    }
    pub(super) fn context_acknowledged(payload: &Payload) -> bool {
        match payload {
            Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::ClientContext(value)),
            }) => value.acknowledged,
            Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::ClientContext(value)),
            }) => value.acknowledged,
            _ => false,
        }
    }
    pub(super) async fn subscription(
        &mut self,
        payload: &Payload,
        principal: VerifiedPrincipal,
        handle: &ServerHandle,
    ) -> Result<bool, ServerFailure> {
        macro_rules! decode {
            ($value:expr) => {{
                let value = $value;
                let fence = identity::decode_fence(
                    value
                        .fence
                        .as_ref()
                        .ok_or_else(|| failure(ServerFailureClass::ScopeMismatch))?,
                    None,
                )
                .map_err(protocol_failure)?;
                let limits = if value.cancel {
                    if value.limits.is_some() {
                        return Err(failure(ServerFailureClass::ScopeMismatch));
                    }
                    None
                } else {
                    let limits = value
                        .limits
                        .as_ref()
                        .ok_or_else(|| failure(ServerFailureClass::ScopeMismatch))?;
                    Some(
                        PageLimits::new(
                            u16::try_from(limits.rows)
                                .map_err(|_| failure(ServerFailureClass::Capacity))?,
                            limits.cells,
                            limits.bytes.min(PAGE_BYTES),
                        )
                        .map_err(|_| failure(ServerFailureClass::Capacity))?,
                    )
                };
                (
                    Token {
                        request: value.request,
                        generation: value.request_generation,
                        fence,
                    },
                    value.cancel,
                    limits,
                )
            }};
        }
        let (token, cancel, limits) = match payload {
            Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::PaneSubscription(value)),
            }) => decode!(value),
            Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::PaneSubscription(value)),
            }) => decode!(value),
            _ => return Ok(false),
        };
        if token.request == 0
            || token.generation == 0
            || token.fence.connection() != principal.binding()
            || token.fence.session().epoch != principal.epoch()
        {
            return Err(failure(ServerFailureClass::ScopeMismatch));
        }
        let pane = token.fence.session().pane;
        if let Some(previous) = self.subscriptions.get(&pane) {
            if cancel {
                if previous.token == token {
                    if let Some(subscription) = self.subscriptions.remove(&pane) {
                        subscription.task.abort();
                    }
                    self.native.retain(|(session, _), _| session.pane != pane);
                }
                return Ok(true);
            }
            if previous.token.request == token.request
                && previous.token.generation >= token.generation
            {
                return Err(failure(ServerFailureClass::StaleAuthority));
            }
        } else if cancel {
            return Ok(true);
        }
        if !self.subscriptions.contains_key(&pane) && self.subscriptions.len() >= MAX_SUBSCRIPTIONS
        {
            return Err(failure(ServerFailureClass::Capacity));
        }
        let mut stream = handle.subscribe_runtime(principal, token.fence).await?;
        let sender = self.sender.clone();
        let task = tokio::spawn(async move {
            loop {
                let event = stream.next().await;
                let ended = event.is_none();
                if sender.send(Event { token, event }).await.is_err() || ended {
                    break;
                }
            }
        })
        .abort_handle();
        let subscription = Subscription {
            token,
            limits: limits.ok_or_else(|| failure(ServerFailureClass::ScopeMismatch))?,
            transfer: 0,
            base: None,
            task,
        };
        if let Some(old) = self.subscriptions.insert(pane, subscription) {
            old.task.abort();
        }
        self.native.retain(|(session, _), _| session.pane != pane);
        Ok(true)
    }
    pub(super) async fn graphics_request(
        &mut self,
        payload: &Payload,
        principal: VerifiedPrincipal,
        handle: &ServerHandle,
    ) -> Result<bool, ServerFailure> {
        self.graphics.admit(payload, principal, handle).await
    }
    pub(super) async fn publish_graphics(
        &mut self,
        event: graphics::GraphicsEvent,
        dispatcher: &ProtocolDispatcher,
        session: &mut AuthenticatedSession,
        transport: &Arc<dyn spaceterm_host::transport::MessageTransport>,
        handle: &ServerHandle,
        egress: &mut ConnectionEgress,
    ) -> Result<(), ServerFailure> {
        self.graphics
            .publish(event, dispatcher, session, transport, handle, egress)
            .await
    }
    pub(super) fn admit_effect(
        &mut self,
        payload: &Payload,
        principal: VerifiedPrincipal,
        handle: &ServerHandle,
        egress: &mut ConnectionEgress,
    ) -> Result<bool, ServerFailure> {
        macro_rules! decode {
            ($value:expr, $p:ident) => {{
                let value = $value;
                (
                    value.request,
                    spaceterm_protocol::convert::runtime::$p::decode_command(value)
                        .map_err(protocol_failure)?,
                )
            }};
        }
        macro_rules! complete {
            ($value:expr, $p:ident) => {{
                let value = $value;
                let fence = identity::decode_fence(value.fence.as_ref().ok_or_else(|| failure(ServerFailureClass::ScopeMismatch))?, None).map_err(protocol_failure)?;
                if value.request == 0 || fence.session().pane.server() != principal.server() || fence.connection().client != principal.client() {
                    return Err(failure(ServerFailureClass::ScopeMismatch));
                }
                let Some(request) = self.native.get(&(fence.session(), value.request)) else {
                    return self.stale_completion(value.request, fence, payload.major(), egress);
                };
                let completion = native_requests::$p::decode_completion(value, request).map_err(protocol_failure)?;
                (value.request, spaceterm_terminal::runtime_contracts::FencedRuntimeCommand { fence, command: spaceterm_terminal::runtime_contracts::RuntimeCommand::CompleteNativeRequest(completion) })
            }};
        }
        self.effect_request(payload)?;
        let (request, command) = match payload {
            Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::RuntimeCommand(value)),
            }) => decode!(value, v1),
            Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::RuntimeCommand(value)),
            }) => decode!(value, v2),
            Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::NativeCompletion(value)),
            }) => complete!(value, v1),
            Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::NativeCompletion(value)),
            }) => complete!(value, v2),
            _ => return Ok(false),
        };
        if self.effects.len() >= 4 {
            return Err(failure(ServerFailureClass::Capacity));
        }
        let reservation = egress.reserve_operation_reply().map_err(egress_failure)?;
        let fence = command.fence;
        let native_completion = matches!(
            command.command,
            spaceterm_terminal::runtime_contracts::RuntimeCommand::CompleteNativeRequest(_)
        );
        let result = handle.try_runtime_command(principal, command)?;
        if native_completion {
            self.native.remove(&(fence.session(), request));
        }
        let major = payload.major();
        self.effect_sequence = self
            .effect_sequence
            .checked_add(1)
            .ok_or_else(|| failure(ServerFailureClass::Exhausted))?;
        let id = self.effect_sequence;
        let sender = self.effect_sender.clone();
        let task = tokio::spawn(async move {
            let response = runtime_failure(result.await).and_then(|failure| match major {
                1 => Ok(Payload::Major1(v1::Message {
                    kind: Some(v1::message::Kind::RuntimeOutcome(v1::RuntimeOutcome {
                        request,
                        fence: Some(identity::encode_fence(fence)),
                        failure,
                    })),
                })),
                2 => Ok(Payload::Major2(v2::Message {
                    kind: Some(v2::message::Kind::RuntimeOutcome(v2::RuntimeOutcome {
                        request,
                        fence: Some(identity::encode_fence(fence)),
                        failure,
                    })),
                })),
                _ => Err(crate::failure::failure(ServerFailureClass::ScopeMismatch)),
            });
            // Authentication/connection loss cannot be turned into an input retry.
            let _ = sender
                .send(EffectReply {
                    response,
                    reservation,
                    id,
                })
                .await;
        })
        .abort_handle();
        self.effects.insert(id, task);
        Ok(true)
    }
    fn stale_completion(
        &mut self,
        request: u64,
        fence: EffectFence,
        major: u32,
        egress: &mut ConnectionEgress,
    ) -> Result<bool, ServerFailure> {
        if self.effects.len() >= 4 {
            return Err(failure(ServerFailureClass::Capacity));
        }
        let reservation = egress.reserve_operation_reply().map_err(egress_failure)?;
        let failure = Some(common::Failure {
            classification: common::FailureClass::Stale as i32,
            retry_after_ms: 0,
        });
        let response = match major {
            1 => Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::RuntimeOutcome(v1::RuntimeOutcome {
                    request,
                    fence: Some(identity::encode_fence(fence)),
                    failure,
                })),
            }),
            2 => Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::RuntimeOutcome(v2::RuntimeOutcome {
                    request,
                    fence: Some(identity::encode_fence(fence)),
                    failure,
                })),
            }),
            _ => return Err(crate::failure::failure(ServerFailureClass::ScopeMismatch)),
        };
        self.effect_sequence = self
            .effect_sequence
            .checked_add(1)
            .ok_or_else(|| crate::failure::failure(ServerFailureClass::Exhausted))?;
        let id = self.effect_sequence;
        let sender = self.effect_sender.clone();
        let task = tokio::spawn(async move {
            let _ = sender
                .send(EffectReply {
                    response: Ok(response),
                    reservation,
                    id,
                })
                .await;
        })
        .abort_handle();
        self.effects.insert(id, task);
        Ok(true)
    }
    pub(super) async fn publish(
        &mut self,
        event: Event,
        dispatcher: &ProtocolDispatcher,
        session: &mut AuthenticatedSession,
        transport: &Arc<dyn spaceterm_host::transport::MessageTransport>,
        handle: &ServerHandle,
        egress: &mut ConnectionEgress,
    ) -> Result<(), ServerFailure> {
        let pane = event.token.fence.session().pane;
        if self
            .subscriptions
            .get(&pane)
            .is_none_or(|subscription| subscription.token != event.token)
        {
            return Ok(());
        }
        if !current(handle, session.principal, event.token.fence).await? {
            self.remove(pane);
            return Ok(());
        }
        let expected_fence = event.token.fence;
        let Some(event) = event.event else {
            self.remove(pane);
            return Ok(());
        };
        let event = match event {
            Ok(event) => event,
            Err(error) if error.class == ServerFailureClass::StaleAuthority => {
                self.remove(pane);
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        match event {
            RuntimePresentationEvent::ContentInvalidated(base) => {
                let subscription = self
                    .subscriptions
                    .get_mut(&pane)
                    .ok_or_else(|| failure(ServerFailureClass::StaleAuthority))?;
                if subscription.base == Some(base) {
                    return Ok(());
                }
                subscription.transfer = subscription
                    .transfer
                    .checked_add(1)
                    .ok_or_else(|| failure(ServerFailureClass::Exhausted))?;
                let stamp = QueryStamp::new(
                    with_content(event_token_fence(subscription), base.revision)?,
                    base,
                    ContentRequestId::new(subscription.token.request)
                        .map_err(|_| failure(ServerFailureClass::ScopeMismatch))?,
                    RequestGeneration::new(subscription.token.generation),
                )
                .map_err(|_| failure(ServerFailureClass::ScopeMismatch))?;
                let base_revision = subscription.base.map_or(0, |base| base.revision.get());
                let mut continuation = None;
                let mut index = 0u32;
                loop {
                    let request = ContentPageRequest::new(
                        stamp,
                        ContentRange::ActiveGrid,
                        continuation,
                        subscription.limits,
                    )
                    .map_err(|_| failure(ServerFailureClass::ScopeMismatch))?;
                    let access = match handle.runtime_access(session.principal, base.session).await
                    {
                        Ok(access) => access,
                        Err(error) if error.class == ServerFailureClass::StaleAuthority => {
                            self.remove(pane);
                            return Ok(());
                        }
                        Err(error) => return Err(error),
                    };
                    let reservation = egress
                        .reserve_content_reply(request.limits().bytes())
                        .map_err(egress_failure)?;
                    let result = access.handle.read_page(request)?.await?;
                    continuation = match &result {
                        ContentReadResult::Ready(page) => page
                            .continuation()
                            .map_err(|_| failure(ServerFailureClass::ScopeMismatch))?,
                        _ => None,
                    };
                    let complete = continuation.is_none();
                    let payload = match session.codec.major() {
                        1 => Payload::Major1(v1::Message {
                            kind: Some(v1::message::Kind::ContentPublication(Box::new(
                                v1::ContentPublication {
                                    request: subscription.token.request,
                                    request_generation: subscription.token.generation,
                                    transfer: subscription.transfer,
                                    index,
                                    base_revision,
                                    response: Some(
                                        content::v1::encode_page_result(&result)
                                            .map_err(protocol_failure)?,
                                    ),
                                    complete,
                                },
                            ))),
                        }),
                        2 => Payload::Major2(v2::Message {
                            kind: Some(v2::message::Kind::ContentPublication(Box::new(
                                v2::ContentPublication {
                                    request: subscription.token.request,
                                    request_generation: subscription.token.generation,
                                    transfer: subscription.transfer,
                                    index,
                                    base_revision,
                                    response: Some(
                                        content::v2::encode_page_result(&result)
                                            .map_err(protocol_failure)?,
                                    ),
                                    complete,
                                },
                            ))),
                        }),
                        _ => return Err(failure(ServerFailureClass::ScopeMismatch)),
                    };
                    if !current(handle, session.principal, subscription.token.fence).await? {
                        self.remove(pane);
                        return Ok(());
                    }
                    dispatcher
                        .send(session, transport, &payload, egress, Some(reservation))
                        .await?;
                    if complete {
                        if matches!(result, ContentReadResult::Ready(_)) {
                            subscription.base = Some(base);
                        }
                        break;
                    }
                    index = index
                        .checked_add(1)
                        .ok_or_else(|| failure(ServerFailureClass::Exhausted))?;
                }
            }
            RuntimePresentationEvent::NativeRequest(request) => {
                let Some(clock) = &self.clock else {
                    // No local tick provider means there is no safe deadline translation.
                    let completion = spaceterm_core::native_requests::NativeCompletion::new(
                        &request,
                        spaceterm_core::native_requests::NativeCompletionKind::Denied,
                    )
                    .map_err(|_| failure(ServerFailureClass::ScopeMismatch))?;
                    let _ = handle.runtime_command(session.principal, spaceterm_terminal::runtime_contracts::FencedRuntimeCommand { fence: request.fence(), command: spaceterm_terminal::runtime_contracts::RuntimeCommand::CompleteNativeRequest(completion) }).await;
                    return Ok(());
                };
                if matches!(
                    request.kind(),
                    spaceterm_core::native_requests::NativeRequestKind::Notification(_)
                ) && !session
                    .codec
                    .capabilities()
                    .contains(&spaceterm_protocol::negotiation::CAPABILITY_NATIVE_NOTIFICATIONS)
                {
                    deny_native(handle, session.principal, &request).await;
                    return Ok(());
                }
                let now = clock.now()?;
                self.native
                    .retain(|_, request| request.deadline().is_none_or(|deadline| deadline > now));
                if request.deadline().is_some_and(|deadline| deadline <= now) {
                    return Ok(());
                }
                if request.fence() != expected_fence
                    || self.native.len() >= MAX_PENDING_NATIVE
                    || !native_current(handle, session.principal, request.fence()).await?
                {
                    deny_native(handle, session.principal, &request).await;
                    return Ok(());
                }
                let text_bytes = match request.kind() {
                    spaceterm_core::native_requests::NativeRequestKind::ClipboardWrite(text)
                    | spaceterm_core::native_requests::NativeRequestKind::UnsafePaste {
                        text,
                        ..
                    } => text.as_str().len(),
                    _ => 0,
                };
                let reservation = match egress.reserve_content_reply((text_bytes + 4096) as u32) {
                    Ok(reservation) => reservation,
                    Err(EgressFailure::Capacity) => {
                        deny_native(handle, session.principal, &request).await;
                        return Ok(());
                    }
                    Err(error) => return Err(egress_failure(error)),
                };
                let payload = match session.codec.major() {
                    1 => Payload::Major1(v1::Message {
                        kind: Some(v1::message::Kind::NativeRequest(
                            native_requests::v1::encode_request(&request, now)
                                .map_err(protocol_failure)?,
                        )),
                    }),
                    2 => Payload::Major2(v2::Message {
                        kind: Some(v2::message::Kind::NativeRequest(
                            native_requests::v2::encode_request(&request, now)
                                .map_err(protocol_failure)?,
                        )),
                    }),
                    _ => return Err(failure(ServerFailureClass::ScopeMismatch)),
                };
                let now = clock.now()?;
                if !native_current(handle, session.principal, request.fence()).await?
                    || request.deadline().is_some_and(|deadline| deadline <= now)
                {
                    deny_native(handle, session.principal, &request).await;
                    return Ok(());
                }
                self.native
                    .insert((request.fence().session(), request.id().get()), request);
                dispatcher
                    .send(session, transport, &payload, egress, Some(reservation))
                    .await?;
            }
            _ => {}
        }
        Ok(())
    }
    fn remove(&mut self, pane: PaneId) {
        self.graphics.remove_pane(pane);
        if let Some(subscription) = self.subscriptions.remove(&pane) {
            subscription.task.abort();
        }
        self.native.retain(|(session, _), _| session.pane != pane);
    }
}
fn event_token_fence(subscription: &Subscription) -> EffectFence {
    subscription.token.fence
}
fn with_content(
    fence: EffectFence,
    revision: ContentRevision,
) -> Result<EffectFence, ServerFailure> {
    EffectFence::new(
        fence.session(),
        fence.grant(),
        fence.connection(),
        fence.context(),
        Some(revision),
    )
    .map_err(|_| failure(ServerFailureClass::ScopeMismatch))
}
async fn current(
    handle: &ServerHandle,
    principal: VerifiedPrincipal,
    fence: EffectFence,
) -> Result<bool, ServerFailure> {
    match handle.runtime_access(principal, fence.session()).await {
        Ok(access) => {
            let parts = access.authority.parts();
            Ok(parts.connection == Some(fence.connection())
                && parts.grant == Some(fence.grant())
                && parts.context == fence.context())
        }
        Err(error) if error.class == ServerFailureClass::StaleAuthority => Ok(false),
        Err(error) => Err(error),
    }
}

async fn native_current(
    handle: &ServerHandle,
    principal: VerifiedPrincipal,
    fence: EffectFence,
) -> Result<bool, ServerFailure> {
    match handle.runtime_access(principal, fence.session()).await {
        Ok(access) => {
            let parts = access.authority.parts();
            Ok(parts.connection == Some(fence.connection())
                && parts.grant == Some(fence.grant())
                && parts.context == fence.context()
                && parts.native_effects_eligible
                && parts.terminal_input_focus)
        }
        Err(error) if error.class == ServerFailureClass::StaleAuthority => Ok(false),
        Err(error) => Err(error),
    }
}

async fn deny_native(
    handle: &ServerHandle,
    principal: VerifiedPrincipal,
    request: &spaceterm_core::native_requests::NativeRequest,
) {
    if let Ok(completion) = spaceterm_core::native_requests::NativeCompletion::new(
        request,
        spaceterm_core::native_requests::NativeCompletionKind::Denied,
    ) {
        let _ = handle.runtime_command(principal, spaceterm_terminal::runtime_contracts::FencedRuntimeCommand { fence: request.fence(), command: spaceterm_terminal::runtime_contracts::RuntimeCommand::CompleteNativeRequest(completion) }).await;
    }
}

pub(super) fn bounded_page_request(
    request: ContentPageRequest,
) -> Result<ContentPageRequest, ServerFailure> {
    let limits = request.limits();
    ContentPageRequest::new(
        request.stamp(),
        request.range(),
        request.continuation(),
        PageLimits::new(
            limits.rows(),
            limits.cells(),
            limits.bytes().min(PAGE_BYTES),
        )
        .map_err(|_| failure(ServerFailureClass::Capacity))?,
    )
    .map_err(|_| failure(ServerFailureClass::ScopeMismatch))
}
pub(super) fn page_reply_budget(payload: &Payload) -> Result<Option<u32>, ServerFailure> {
    let request = match payload {
        Payload::Major1(v1::Message {
            kind: Some(v1::message::Kind::ContentQuery(value)),
        }) => content::v1::decode_page_request(value).map_err(protocol_failure)?,
        Payload::Major2(v2::Message {
            kind: Some(v2::message::Kind::ContentQuery(value)),
        }) => content::v2::decode_page_request(value).map_err(protocol_failure)?,
        _ => return Ok(None),
    };
    Ok(Some(bounded_page_request(request)?.limits().bytes()))
}
