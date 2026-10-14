//! Connection-owned context, subscription and bounded semantic read delivery.

use super::*;
use spaceterm_client::pane_channel::{NativeTiming, PaneChannel, PaneReadItem};
use spaceterm_core::{content_queries::*, observations::MonotonicTick};
use spaceterm_protocol::convert::{
    content::{
        self,
        reads::{self, ReadQuery},
    },
    native_requests,
};
use std::time::Duration;
use tokio::sync::{Notify, mpsc};

#[derive(Clone, Copy, Eq, PartialEq)]
struct Context {
    binding: ConnectionBinding,
    generation: ContextGeneration,
    focused: Option<PaneId>,
    input: bool,
    native: bool,
}
struct PendingContext {
    context: Context,
    reply: oneshot::Sender<Result<(), ClientFailure>>,
}
struct Subscription {
    fence: EffectFence,
    request: u64,
    generation: u64,
    transfer: u64,
    index: u32,
    projection_stamp: Option<QueryStamp>,
}
struct PendingRead {
    permit: EffectPermit,
    request: PaneReadRequest,
    sender: mpsc::Sender<Result<PaneReadItem, ClientFailure>>,
}
struct CancelledRead {
    stamp: QueryStamp,
    resource: Option<ResourceTransferId>,
}
struct PasteCompletion {
    confirm: spaceterm_core::native_requests::NativeCompletion,
    cancel: spaceterm_core::native_requests::NativeCompletion,
    expires: tokio::time::Instant,
}
#[derive(Default)]
struct State {
    synchronizers: BTreeMap<ServerId, Arc<tokio::sync::Mutex<()>>>,
    contexts: BTreeMap<ServerId, Context>,
    pending_contexts: BTreeMap<(ServerId, u64), PendingContext>,
    subscriptions: BTreeMap<PaneId, Subscription>,
    reads: BTreeMap<(ServerId, u64), PendingRead>,
    cancellations: BTreeMap<(ServerId, u64), CancelledRead>,
    native_requests: BTreeMap<(SessionKey, u64), PasteCompletion>,
}
#[derive(Default)]
pub(super) struct TerminalTransport {
    state: Mutex<State>,
    pub(super) changed: Arc<Notify>,
    pub(super) effects_changed: Arc<Notify>,
}
impl TerminalTransport {
    pub(super) fn invalidate_publication(&self, server: ServerId) {
        if let Ok(mut state) = self.state.lock() {
            // Organization publication replaces content authority, not the
            // connection's acknowledged native context or an in-flight update.
            state
                .subscriptions
                .retain(|pane, _| pane.server() != server);
            state.reads.retain(|(owner, _), _| *owner != server);
        }
    }
    pub(super) fn invalidate(&self, server: ServerId) {
        if let Ok(mut state) = self.state.lock() {
            state.synchronizers.remove(&server);
            state.contexts.remove(&server);
            state
                .pending_contexts
                .retain(|(owner, _), _| *owner != server);
            state
                .subscriptions
                .retain(|pane, _| pane.server() != server);
            state.reads.retain(|(owner, _), _| *owner != server);
            state.cancellations.retain(|(owner, _), _| *owner != server);
            state
                .native_requests
                .retain(|(session, _), _| session.pane.server() != server);
        }
    }
}
impl NativeProtocol {
    pub(super) fn paste_completion(
        &self,
        fence: EffectFence,
        approval: spaceterm_core::native_requests::PasteApprovalId,
        decision: spaceterm_core::native_requests::PasteDecision,
    ) -> Result<spaceterm_core::native_requests::NativeCompletion, ClientFailure> {
        let mut state = self.terminal.state.lock().map_err(|_| invalid())?;
        state
            .native_requests
            .retain(|_, pending| tokio::time::Instant::now() < pending.expires);
        let pending = state.native_requests.values().find(|pending| {
            pending.confirm.fence() == fence && matches!(pending.confirm.kind(),
                spaceterm_core::native_requests::NativeCompletionKind::Paste { approval: expected, .. } if *expected == approval)
        }).ok_or_else(stale)?;
        Ok(match decision {
            spaceterm_core::native_requests::PasteDecision::Confirm => pending.confirm.clone(),
            spaceterm_core::native_requests::PasteDecision::Cancel => pending.cancel.clone(),
        })
    }
    fn remember_paste(
        &self,
        request: &spaceterm_core::native_requests::NativeRequest,
        timing: NativeTiming,
    ) -> Result<(), ClientFailure> {
        use spaceterm_core::native_requests::{
            NativeCompletion, NativeCompletionKind, NativeRequestKind, PasteDecision,
        };
        let mut state = self.terminal.state.lock().map_err(|_| invalid())?;
        state
            .native_requests
            .retain(|_, pending| tokio::time::Instant::now() < pending.expires);
        if state
            .native_requests
            .contains_key(&(request.fence().session(), request.id().get()))
        {
            return Err(stale());
        }
        if state.native_requests.len() >= 64 {
            return Err(fail(ClientFailureClass::ResourceLimit));
        }
        let NativeRequestKind::UnsafePaste { approval, .. } = request.kind() else {
            return Err(invalid());
        };
        let completion = |decision| {
            NativeCompletion::new(
                request,
                NativeCompletionKind::Paste {
                    approval: *approval,
                    decision,
                },
            )
            .map_err(|_| invalid())
        };
        let expires = tokio::time::Instant::now()
            .checked_add(
                timing
                    .expires_after
                    .unwrap_or_default()
                    .min(Duration::from_secs(30)),
            )
            .ok_or_else(invalid)?;
        // Publish correlation metadata before exposing the permission to a subscriber.
        // A fast resolution cannot race the registration, and this map holds no paste text.
        state.native_requests.insert(
            (request.fence().session(), request.id().get()),
            PasteCompletion {
                confirm: completion(PasteDecision::Confirm)?,
                cancel: completion(PasteDecision::Cancel)?,
                expires,
            },
        );
        Ok(())
    }
    pub(super) fn forget_native(
        &self,
        session: SessionKey,
        request: u64,
    ) -> Result<(), ClientFailure> {
        self.terminal
            .state
            .lock()
            .map_err(|_| invalid())?
            .native_requests
            .remove(&(session, request));
        Ok(())
    }
    pub(super) fn start_presentation(self: &Arc<Self>) {
        self.start_effect_dispatch();
        let owner = Arc::downgrade(self);
        let changed = self.terminal.changed.clone();
        self.executor.spawn(async move {
            loop {
                changed.notified().await;
                let Some(owner) = owner.upgrade() else { break };
                let infos = match owner.infos.lock() {
                    Ok(infos) => infos.values().copied().collect::<Vec<_>>(),
                    Err(_) => break,
                };
                for info in infos {
                    if let Err(error) = owner.synchronize_presentation(info).await {
                        // Stale demand is disposable. A connection failure is published
                        // by the registry, which revokes its subscriptions and reads.
                        if error.class() == ClientFailureClass::ResourceLimit {
                            // A busy account listener has not acknowledged this demand.
                            // Retry without discarding the last acknowledged context.
                            tokio::time::sleep(Duration::from_millis(20)).await;
                            changed.notify_one();
                        } else if error.class() != ClientFailureClass::StaleAuthority {
                            // Local presentation or capacity failures do not
                            // revoke an acknowledged connection context.
                            owner.terminal.invalidate_publication(info.server);
                        } else if owner.handle(info.server).is_ok_and(|handle| {
                            !matches!(*handle.state().borrow(), ConnectionState::Connected { .. })
                        }) {
                            tokio::time::sleep(Duration::from_millis(20)).await;
                            changed.notify_one();
                        }
                    }
                }
            }
        });
    }
    fn start_effect_dispatch(self: &Arc<Self>) {
        let owner = Arc::downgrade(self);
        let changed = self.terminal.effects_changed.clone();
        self.executor.spawn(async move {
            loop {
                changed.notified().await;
                let Some(owner) = owner.upgrade() else { break };
                loop {
                    let queued = {
                        let Ok(mut effects) = owner.effects.lock() else {
                            return;
                        };
                        effects
                            .iter_mut()
                            .filter(|(_, effect)| effect.payload.is_some())
                            .min_by_key(|(_, effect)| effect.order)
                            .and_then(|(key, effect)| {
                                effect.payload.take().map(|payload| {
                                    (*key, effect.info, effect.permit.clone(), payload)
                                })
                            })
                    };
                    let Some((key, info, permit, payload)) = queued else {
                        break;
                    };
                    let result = async {
                        if !permit.is_current() {
                            return Err(stale());
                        }
                        owner.synchronize_fence(permit.fence()).await?;
                        owner.handle(info.server)?.dispatch(info, payload).await
                    }
                    .await;
                    if result.is_err()
                        && let Ok(mut effects) = owner.effects.lock()
                        && effects
                            .get(&key)
                            .is_some_and(|effect| effect.permit.fence() == permit.fence())
                    {
                        effects.remove(&key);
                    }
                }
            }
        });
    }
    pub(super) async fn synchronize_presentation(
        &self,
        info: ConnectedInfo,
    ) -> Result<(), ClientFailure> {
        let synchronized = {
            let mut state = self.terminal.state.lock().map_err(|_| invalid())?;
            state.synchronizers.entry(info.server).or_default().clone()
        };
        let _serialized = synchronized.lock().await;
        let (handle, current) = self.connected(info.server)?;
        if current.binding != info.binding {
            return Err(stale());
        }
        if !info
            .capabilities
            .supports(spaceterm_protocol::negotiation::CAPABILITY_TERMINAL_STREAMS)
        {
            return Ok(());
        }
        let channels = self.channels()?;
        let panes = channels.presentation_channels(info.server)?;
        let previous = self
            .terminal
            .state
            .lock()
            .map_err(|_| invalid())?
            .contexts
            .get(&info.server)
            .copied();
        let maximum = panes.iter().map(|pane| pane.state().borrow().context).max();
        let Some(generation) = maximum.or_else(|| previous.map(|old| old.generation)) else {
            return Ok(());
        };
        let focused = panes
            .iter()
            .filter(|pane| pane.state().borrow().facts.focused_pane)
            .max_by_key(|pane| pane.state().borrow().context);
        let facts = focused
            .map(|pane| pane.state().borrow().facts)
            .unwrap_or_default();
        let mut context = Context {
            binding: info.binding,
            generation,
            focused: focused.map(PaneChannel::pane),
            input: facts.terminal_input_focus,
            native: facts.presentable && facts.terminal_input_focus,
        };
        if let Some(previous) = previous
            && previous != context
            && context.generation <= previous.generation
        {
            context.generation = channels.next_connection_context()?;
        }
        channels.align_connection_context(info.server, context.generation)?;
        if previous != Some(context) {
            let request = self.next()?;
            let (reply, received) = oneshot::channel();
            self.terminal
                .state
                .lock()
                .map_err(|_| invalid())?
                .pending_contexts
                .insert((info.server, request), PendingContext { context, reply });
            let payload = context_payload(info.major, request, info.epoch, context)?;
            let result = async {
                handle.dispatch(info, payload).await?;
                tokio::time::timeout(Duration::from_secs(15), received)
                    .await
                    .map_err(|_| fail(ClientFailureClass::Unavailable))?
                    .map_err(|_| stale())?
            }
            .await;
            if let Err(error) = result {
                self.terminal
                    .state
                    .lock()
                    .map_err(|_| invalid())?
                    .pending_contexts
                    .remove(&(info.server, request));
                return Err(error);
            }
            self.terminal
                .state
                .lock()
                .map_err(|_| invalid())?
                .contexts
                .insert(info.server, context);
        }
        let mut wanted = BTreeMap::new();
        for pane in &panes {
            let state = *pane.state().borrow();
            let Some(runtime) = state.runtime else {
                continue;
            };
            if !state.facts.presentable || !matches!(runtime.phase, SessionPhase::Running { .. }) {
                continue;
            }
            let ControlRecord::Held { client, generation } = runtime.control else {
                continue;
            };
            if client != info.client {
                continue;
            }
            let fence = EffectFence::new(
                SessionKey {
                    pane: pane.pane(),
                    epoch: info.epoch,
                    generation: runtime.session_generation,
                },
                ControlGrant::new(pane.pane(), client, generation).map_err(|_| invalid())?,
                info.binding,
                context.generation,
                None,
            )
            .map_err(|_| invalid())?;
            wanted.insert(pane.pane(), fence);
        }
        let removed = {
            let mut state = self.terminal.state.lock().map_err(|_| invalid())?;
            let obsolete = state
                .subscriptions
                .iter()
                .filter(|(pane, subscription)| {
                    pane.server() == info.server && wanted.get(pane) != Some(&subscription.fence)
                })
                .map(|(pane, _)| *pane)
                .collect::<Vec<_>>();
            obsolete
                .into_iter()
                .filter_map(|pane| state.subscriptions.remove(&pane))
                .collect::<Vec<_>>()
        };
        for subscription in removed {
            // Cancellation has no runtime or control effect. It uses the original
            // token even if today's connection context has moved.
            handle
                .dispatch(info, subscription_payload(info.major, &subscription, true)?)
                .await?;
        }
        for (pane, fence) in wanted {
            let subscription = {
                let mut state = self.terminal.state.lock().map_err(|_| invalid())?;
                if state.subscriptions.contains_key(&pane) {
                    continue;
                }
                if state.subscriptions.len() >= 64 {
                    return Err(fail(ClientFailureClass::ResourceLimit));
                }
                let request = self.next()?;
                let subscription = Subscription {
                    fence,
                    request,
                    generation: context.generation.get(),
                    transfer: 0,
                    index: 0,
                    projection_stamp: None,
                };
                let payload = subscription_payload(info.major, &subscription, false)?;
                state.subscriptions.insert(pane, subscription);
                payload
            };
            if let Err(error) = handle.dispatch(info, subscription).await {
                self.terminal
                    .state
                    .lock()
                    .map_err(|_| invalid())?
                    .subscriptions
                    .remove(&pane);
                return Err(error);
            }
        }
        Ok(())
    }
    pub(super) async fn synchronize_fence(&self, fence: EffectFence) -> Result<(), ClientFailure> {
        let (_, info) = self.connected(fence.session().pane.server())?;
        self.synchronize_presentation(info).await?;
        let state = self.terminal.state.lock().map_err(|_| invalid())?;
        if state.contexts.get(&info.server).is_none_or(|context| {
            context.binding != fence.connection() || context.generation != fence.context()
        }) {
            return Err(stale());
        }
        Ok(())
    }
    pub(super) fn admit_terminal(
        &self,
        info: ConnectedInfo,
        payload: &Payload,
    ) -> Result<OutboundClass, ClientFailure> {
        macro_rules! profile {
            ($message:expr, $p:ident) => {{
                let state = self.terminal.state.lock().map_err(|_| invalid())?;
                match $message.kind.as_ref().ok_or_else(invalid)? {
                    $p::message::Kind::ClientContext(value) => {
                        let expected = &state
                            .pending_contexts
                            .get(&(info.server, value.request))
                            .ok_or_else(stale)?
                            .context;
                        if value.acknowledged
                            || value.failure.is_some()
                            || value.epoch != info.epoch.get()
                            || value.connection_generation != info.binding.generation.get()
                            || value.context_generation != expected.generation.get()
                            || value.focused_pane != expected.focused.map(identity::encode_pane)
                            || value.terminal_input_focus != expected.input
                            || value.native_effects_eligible != expected.native
                        {
                            return Err(stale());
                        }
                        Ok(OutboundClass::Lifecycle)
                    }
                    $p::message::Kind::PaneSubscription(value) => {
                        let fence =
                            identity::decode_fence(value.fence.as_ref().ok_or_else(invalid)?, None)
                                .map_err(ClientFailure::from)?;
                        if value.cancel {
                            return Ok(OutboundClass::Lifecycle);
                        }
                        let subscription = state
                            .subscriptions
                            .get(&fence.session().pane)
                            .ok_or_else(stale)?;
                        if subscription.request != value.request
                            || subscription.generation != value.request_generation
                            || subscription.fence != fence
                        {
                            return Err(stale());
                        }
                        Ok(OutboundClass::Ordinary)
                    }
                    $p::message::Kind::ContentQuery(value) => {
                        if matches!(value.kind, 3..=5) {
                            let query =
                                reads::$p::decode_query(value).map_err(ClientFailure::from)?;
                            if let ReadQuery::Cancel { stamp, .. } = query {
                                if state
                                    .cancellations
                                    .get(&(info.server, stamp.request().get()))
                                    .map(|read| read.stamp)
                                    != Some(stamp)
                                    || stamp.fence().connection() != info.binding
                                {
                                    return Err(stale());
                                }
                                return Ok(OutboundClass::Lifecycle);
                            }
                            admit_read(&state, info.server, query.stamp())
                        } else {
                            let request = content::$p::decode_page_request(value)
                                .map_err(ClientFailure::from)?;
                            admit_read(&state, info.server, request.stamp())
                        }
                    }
                    $p::message::Kind::ResourceCancel(value) => {
                        if !state.cancellations.values().any(|read| {
                            read.resource
                                .is_some_and(|resource| resource.get() == value.transfer)
                                && read.stamp.base().session.pane.server() == info.server
                                && read.stamp.fence().connection() == info.binding
                                && value.fence.as_ref()
                                    == Some(&identity::encode_fence(read.stamp.fence()))
                        }) {
                            return Err(stale());
                        }
                        Ok(OutboundClass::Lifecycle)
                    }
                    $p::message::Kind::ResourceRequest(value) => {
                        let request = content::$p::decode_graphics_request(value)
                            .map_err(ClientFailure::from)?;
                        admit_read(&state, info.server, request.stamp())
                    }
                    _ => Err(fail(ClientFailureClass::MissingCapability)),
                }
            }};
        }
        match payload {
            Payload::Major1(message) => profile!(message, v1),
            Payload::Major2(message) => profile!(message, v2),
        }
    }
    pub(super) async fn receive_terminal(
        &self,
        info: ConnectedInfo,
        payload: &Payload,
    ) -> Result<bool, ClientFailure> {
        macro_rules! profile {
            ($message:expr, $p:ident) => {{
                match $message.kind.as_ref().ok_or_else(invalid)? {
                    $p::message::Kind::ClientContext(value) => {
                        let pending = self
                            .terminal
                            .state
                            .lock()
                            .map_err(|_| invalid())?
                            .pending_contexts
                            .remove(&(info.server, value.request))
                            .ok_or_else(stale)?;
                        let context = pending.context;
                        if value.acknowledged != value.failure.is_none()
                            || value.epoch != info.epoch.get()
                            || value.connection_generation != context.binding.generation.get()
                            || value.context_generation != context.generation.get()
                            || value.focused_pane != context.focused.map(identity::encode_pane)
                            || value.terminal_input_focus != context.input
                            || value.native_effects_eligible != context.native
                        {
                            return Err(invalid());
                        }
                        let result = match value.failure.as_ref() {
                            None => Ok(()),
                            Some(failure)
                                if failure.classification == spaceterm_protocol::wire::common::FailureClass::Busy as i32 =>
                            {
                                Err(fail(ClientFailureClass::ResourceLimit))
                            }
                            Some(failure)
                                if failure.classification == spaceterm_protocol::wire::common::FailureClass::Stale as i32 =>
                            {
                                Err(stale())
                            }
                            Some(_) => return Err(invalid()),
                        };
                        let _ = pending.reply.send(result);
                        Ok(true)
                    }
                    $p::message::Kind::ContentPublication(value) => {
                        let (fence, limits) = {
                            let mut state = self.terminal.state.lock().map_err(|_| invalid())?;
                            let Some(subscription) =
                                state.subscriptions.values_mut().find(|subscription| {
                                    subscription.request == value.request
                                        && subscription.fence.session().pane.server() == info.server
                                })
                            else {
                                return Ok(true);
                            };
                            if value.request_generation != subscription.generation {
                                return Ok(true);
                            }
                            if value.transfer > subscription.transfer {
                                if value.index != 0
                                    || value.transfer
                                        != subscription
                                            .transfer
                                            .checked_add(1)
                                            .ok_or_else(invalid)?
                                {
                                    return Err(invalid());
                                }
                                subscription.transfer = value.transfer;
                                subscription.index = 0;
                                subscription.projection_stamp = None;
                            }
                            if value.transfer != subscription.transfer
                                || value.index != subscription.index
                            {
                                return Err(invalid());
                            }
                            subscription.index =
                                subscription.index.checked_add(1).ok_or_else(invalid)?;
                            (subscription.fence, page_limits()?)
                        };
                        let response = value.response.as_ref().ok_or_else(invalid)?;
                        let $p::content_read_response::Result::Page(page) =
                            response.result.as_ref().ok_or_else(invalid)?
                        else {
                            return Ok(true);
                        };
                        let stamp =
                            content::$p::decode_stamp(page.stamp.as_ref().ok_or_else(invalid)?)
                                .map_err(ClientFailure::from)?;
                        if stamp.request().get() != value.request
                            || stamp.generation().get() != value.request_generation
                            || !fence.is_current(
                                &EffectFence::new(
                                    stamp.fence().session(),
                                    stamp.fence().grant(),
                                    stamp.fence().connection(),
                                    stamp.fence().context(),
                                    None,
                                )
                                .map_err(|_| invalid())?,
                            )
                        {
                            return Err(invalid());
                        }
                        let request =
                            ContentPageRequest::new(stamp, ContentRange::ActiveGrid, None, limits)
                                .map_err(|_| invalid())?;
                        let reply = content::$p::decode_page(page, &request)
                            .map_err(ClientFailure::from)?;
                        if value.complete != reply.continuation().map_err(|_| invalid())?.is_none()
                            || reply.page().accounted_bytes() > limits.bytes() as usize
                        {
                            return Err(invalid());
                        }
                        let Some(pane) =
                            self.presentation_pane(info.server, fence.session().pane)?
                        else {
                            return Ok(true);
                        };
                        if pane.publish_content_base(fence, stamp.base()).is_err() {
                            return Ok(true);
                        }
                        let projection_stamp = {
                            let mut state = self.terminal.state.lock().map_err(|_| invalid())?;
                            let subscription = state
                                .subscriptions
                                .get_mut(&fence.session().pane)
                                .ok_or_else(stale)?;
                            match subscription.projection_stamp {
                                Some(stamp) => stamp,
                                None => {
                                    let permit = pane
                                        .permit(spaceterm_client::pane_channel::EffectKind::Read)?;
                                    let stamp = pane.query_stamp(
                                        &permit,
                                        RequestGeneration::new(value.transfer),
                                    )?;
                                    subscription.projection_stamp = Some(stamp);
                                    stamp
                                }
                            }
                        };
                        pane.projection()
                            .lock()
                            .map_err(|_| invalid())?
                            .accept_page(projection_stamp, reply.page().clone())?;
                        pane.publish_content_base(fence, stamp.base())?;
                        Ok(true)
                    }
                    $p::message::Kind::ContentReadResponse(response) => {
                        let stamp = match response.result.as_ref().ok_or_else(invalid)? {
                            $p::content_read_response::Result::Page(page) => {
                                content::$p::decode_stamp(page.stamp.as_ref().ok_or_else(invalid)?)
                            }
                            $p::content_read_response::Result::ResyncRequired(value) => {
                                content::$p::decode_stamp(value.stamp.as_ref().ok_or_else(invalid)?)
                            }
                            $p::content_read_response::Result::Rejected(value) => {
                                content::$p::decode_stamp(value.stamp.as_ref().ok_or_else(invalid)?)
                            }
                            $p::content_read_response::Result::Topology(page) => {
                                content::$p::decode_stamp(page.stamp.as_ref().ok_or_else(invalid)?)
                            }
                        }
                        .map_err(ClientFailure::from)?;
                        let pending = self
                            .terminal
                            .state
                            .lock()
                            .map_err(|_| invalid())?
                            .reads
                            .remove(&(info.server, stamp.request().get()));
                        let Some(pending) = pending else {
                            return Ok(true);
                        };
                        if stamp != pending.request.stamp() {
                            return Err(invalid());
                        }
                        let item =
                            match pending.request {
                                PaneReadRequest::Page(request) => {
                                    PaneReadItem::Page(decode_result!(response, request, $p)?)
                                }
                                PaneReadRequest::Topology(request) => PaneReadItem::Topology(
                                    reads::$p::decode_topology_result(response, &request)
                                        .map_err(ClientFailure::from)?,
                                ),
                                PaneReadRequest::Graphics(_) => PaneReadItem::Graphics(
                                    match response.result.as_ref().ok_or_else(invalid)? {
                                        $p::content_read_response::Result::ResyncRequired(
                                            value,
                                        ) => ContentReadResult::ResyncRequired {
                                            request: stamp,
                                            current: content::$p::decode_base(
                                                value.current.as_ref().ok_or_else(invalid)?,
                                            )
                                            .map_err(ClientFailure::from)?,
                                        },
                                        $p::content_read_response::Result::Rejected(value) => {
                                            ContentReadResult::Rejected {
                                                request: stamp,
                                                reason: read_failure(value.reason)?,
                                            }
                                        }
                                        _ => return Err(invalid()),
                                    },
                                ),
                                _ => return Err(invalid()),
                            };
                        if pending.permit.is_current() {
                            pending
                                .sender
                                .try_send(Ok(item))
                                .map_err(|_| fail(ClientFailureClass::ResourceLimit))?;
                        }
                        Ok(true)
                    }
                    $p::message::Kind::CopyEvent(value) => {
                        let stamp = match value.item.as_ref().ok_or_else(invalid)? {
                            $p::copy_event::Item::Chunk(value) => value.stamp.as_ref(),
                            $p::copy_event::Item::Complete(value) => value.stamp.as_ref(),
                            $p::copy_event::Item::ContentFailure(value) => value.stamp.as_ref(),
                            _ => return Err(invalid()),
                        }
                        .ok_or_else(invalid)?;
                        let stamp =
                            content::$p::decode_stamp(stamp).map_err(ClientFailure::from)?;
                        self.deliver_read(info.server, stamp, |request| {
                            let PaneReadRequest::Copy(request) = request else {
                                return Err(invalid());
                            };
                            let item = reads::$p::decode_copy_event(value, request)
                                .map_err(ClientFailure::from)?;
                            let complete = !matches!(item, CopyStreamItem::Chunk(_));
                            Ok((PaneReadItem::Copy(item), complete))
                        })
                        .await?;
                        Ok(true)
                    }
                    $p::message::Kind::FindEvent(value) => {
                        let stamp = match value.item.as_ref().ok_or_else(invalid)? {
                            $p::find_event::Item::Chunk(value) => value.stamp.as_ref(),
                            $p::find_event::Item::Complete(value) => value.stamp.as_ref(),
                            $p::find_event::Item::ContentFailure(value) => value.stamp.as_ref(),
                            _ => return Err(invalid()),
                        }
                        .ok_or_else(invalid)?;
                        let stamp =
                            content::$p::decode_stamp(stamp).map_err(ClientFailure::from)?;
                        self.deliver_read(info.server, stamp, |request| {
                            let PaneReadRequest::Find(request) = request else {
                                return Err(invalid());
                            };
                            let item = reads::$p::decode_find_event(value, request)
                                .map_err(ClientFailure::from)?;
                            let complete = !matches!(item, FindStreamItem::Chunk(_));
                            Ok((PaneReadItem::Find(item), complete))
                        })
                        .await?;
                        Ok(true)
                    }
                    $p::message::Kind::ResourceChunk(value) => {
                        let stamp =
                            content::$p::decode_stamp(value.stamp.as_ref().ok_or_else(invalid)?)
                                .map_err(ClientFailure::from)?;
                        let delivery = {
                            let mut state = self.terminal.state.lock().map_err(|_| invalid())?;
                            let Some(pending) =
                                state.reads.get(&(info.server, stamp.request().get()))
                            else {
                                return Ok(true);
                            };
                            let PaneReadRequest::Graphics(request) = pending.request else {
                                return Err(invalid());
                            };
                            let chunk = content::$p::decode_graphics_chunk(value, request)
                                .map_err(ClientFailure::from)?;
                            let delivery = pending
                                .permit
                                .is_current()
                                .then(|| (pending.sender.clone(), chunk));
                            if value.complete {
                                state.reads.remove(&(info.server, stamp.request().get()));
                            }
                            delivery
                        };
                        if let Some((sender, chunk)) = delivery {
                            // The authenticated receiver applies bounded backpressure,
                            // rather than dropping a valid resource stream at chunk 3.
                            let _ = sender
                                .send(Ok(PaneReadItem::Graphics(ContentReadResult::Ready(chunk))))
                                .await;
                        }
                        Ok(true)
                    }
                    $p::message::Kind::NativeRequest(value) => {
                        let request =
                            native_requests::$p::decode_request(value, MonotonicTick::new(0))
                                .map_err(ClientFailure::from)?;
                        let Some(pane) =
                            self.presentation_pane(info.server, request.fence().session().pane)?
                        else {
                            return Ok(true);
                        };
                        let timing = NativeTiming {
                            not_before: Duration::from_millis(u64::from(value.not_before_delay_ms)),
                            expires_after: value
                                .remaining_ms
                                .map(|value| Duration::from_millis(u64::from(value))),
                        };
                        let correlation = if matches!(request.kind(), spaceterm_core::native_requests::NativeRequestKind::UnsafePaste { .. }) {
                            let Some(origin) = request.runtime_request() else {
                                // An older provider cannot prove which paste requested this permission.
                                // Suppress it instead of guessing from the native request number.
                                return Ok(true);
                            };
                            Some(EffectCorrelation::new(origin.get(), request.fence())?)
                        } else { None };
                        if correlation.is_some() {
                            match self.remember_paste(&request, timing) {
                                Ok(()) => {},
                                Err(error) if error.class() == ClientFailureClass::StaleAuthority => return Ok(true),
                                Err(error) => return Err(error),
                            }
                        }
                        let received = match correlation {
                            Some(correlation) => pane.receive_paste_approval(correlation, request.clone(), timing),
                            None => pane.receive_native(request.clone(), timing),
                        };
                        if received.is_err() && correlation.is_some() {
                            self.forget_native(request.fence().session(), request.id().get())?;
                        }
                        match received {
                            Ok(()) => Ok(true),
                            Err(error) if error.class() == ClientFailureClass::StaleAuthority => Ok(true),
                            Err(error) => Err(error),
                        }
                    }
                    _ => Ok(false),
                }
            }};
        }
        let result = async {
            match payload {
                Payload::Major1(message) => profile!(message, v1),
                Payload::Major2(message) => profile!(message, v2),
            }
        }
        .await;
        match result {
            Err(error) if error.class() == ClientFailureClass::StaleAuthority => Ok(true),
            result => result,
        }
    }
    async fn deliver_read(
        &self,
        server: ServerId,
        stamp: QueryStamp,
        decode: impl FnOnce(&PaneReadRequest) -> Result<(PaneReadItem, bool), ClientFailure>,
    ) -> Result<(), ClientFailure> {
        let delivery = {
            let mut state = self.terminal.state.lock().map_err(|_| invalid())?;
            let key = (server, stamp.request().get());
            let Some(pending) = state.reads.get(&key) else {
                return Ok(());
            };
            if pending.request.stamp() != stamp {
                return Err(invalid());
            }
            let (item, complete) = decode(&pending.request)?;
            let delivery = pending
                .permit
                .is_current()
                .then(|| (pending.sender.clone(), item));
            if complete {
                state.reads.remove(&key);
            }
            delivery
        };
        if let Some((sender, item)) = delivery {
            let _ = sender.send(Ok(item)).await;
        }
        Ok(())
    }
    fn cancel_read(self: &Arc<Self>, request: PaneReadRequest) {
        let kind = match request {
            PaneReadRequest::Topology(_) => 3,
            PaneReadRequest::Find(_) => 4,
            PaneReadRequest::Copy(_) => 5,
            PaneReadRequest::Graphics(_) => 0,
            // Page replies are single-frame and have no retained server demand.
            PaneReadRequest::Page(_) => return,
        };
        let stamp = request.stamp();
        let server = stamp.base().session.pane.server();
        let Ok((handle, info)) = self.connected(server) else {
            return;
        };
        if info.binding != stamp.fence().connection() {
            return;
        }
        let key = (server, stamp.request().get());
        {
            let Ok(mut state) = self.terminal.state.lock() else {
                return;
            };
            if state.cancellations.len() >= spaceterm_protocol::limits::MAX_EGRESS_ENVELOPES {
                return;
            }
            state.cancellations.insert(
                key,
                CancelledRead {
                    stamp,
                    resource: match request {
                        PaneReadRequest::Graphics(request) => Some(request.transfer()),
                        _ => None,
                    },
                },
            );
        }
        let payload = if let PaneReadRequest::Graphics(request) = request {
            match info.major {
                1 => Payload::Major1(v1::Message {
                    kind: Some(v1::message::Kind::ResourceCancel(v1::ResourceCancel {
                        transfer: request.transfer().get(),
                        fence: Some(identity::encode_fence(stamp.fence())),
                    })),
                }),
                2 => Payload::Major2(v2::Message {
                    kind: Some(v2::message::Kind::ResourceCancel(v2::ResourceCancel {
                        transfer: request.transfer().get(),
                        fence: Some(identity::encode_fence(stamp.fence())),
                    })),
                }),
                _ => return,
            }
        } else {
            let query = ReadQuery::Cancel { stamp, kind };
            match info.major {
                1 => Payload::Major1(v1::Message {
                    kind: Some(v1::message::Kind::ContentQuery(reads::v1::encode_query(
                        &query,
                    ))),
                }),
                2 => Payload::Major2(v2::Message {
                    kind: Some(v2::message::Kind::ContentQuery(reads::v2::encode_query(
                        &query,
                    ))),
                }),
                _ => return,
            }
        };
        let owner = Arc::downgrade(self);
        self.executor.spawn(async move {
            let _ = handle.dispatch(info, payload).await;
            if let Some(owner) = owner.upgrade()
                && let Ok(mut state) = owner.terminal.state.lock()
                && state.cancellations.get(&key).map(|read| read.stamp) == Some(stamp)
            {
                state.cancellations.remove(&key);
            }
        });
    }
    pub(super) fn presentation_pane(
        &self,
        server: ServerId,
        pane: PaneId,
    ) -> Result<Option<PaneChannel>, ClientFailure> {
        Ok(self
            .channels()?
            .presentation_channels(server)?
            .into_iter()
            .find(|channel| channel.pane() == pane))
    }
    pub(super) async fn read_terminal(
        &self,
        permit: EffectPermit,
        request: PaneReadRequest,
    ) -> Result<Box<dyn PaneReadSource>, ClientFailure> {
        if !permit.is_current() || permit.fence() != request.stamp().fence() {
            return Err(stale());
        }
        self.synchronize_fence(permit.fence()).await?;
        if !permit.is_current() {
            return Err(stale());
        }
        let server = permit.fence().session().pane.server();
        let (handle, info) = self.connected(server)?;
        let payload = read_payload(info.major, &request)?;
        let key = (server, request.stamp().request().get());
        let (sender, receiver) = mpsc::channel(2);
        {
            let mut state = self.terminal.state.lock().map_err(|_| invalid())?;
            state
                .reads
                .retain(|_, read| read.permit.is_current() && !read.sender.is_closed());
            if state.reads.len() >= info.limits.concurrent_transfers
                || state.reads.len().saturating_add(state.cancellations.len())
                    >= spaceterm_protocol::limits::MAX_EGRESS_ENVELOPES
                || state.reads.contains_key(&key)
                || state.cancellations.contains_key(&key)
            {
                return Err(fail(ClientFailureClass::ResourceLimit));
            }
            state.reads.insert(
                key,
                PendingRead {
                    permit,
                    request,
                    sender,
                },
            );
        }
        if let Err(error) = handle.dispatch(info, payload).await {
            self.terminal
                .state
                .lock()
                .map_err(|_| invalid())?
                .reads
                .remove(&key);
            return Err(error);
        }
        Ok(Box::new(ReadSource {
            receiver,
            owner: self.owner.get().cloned().ok_or_else(stale)?,
            key,
        }))
    }
}

macro_rules! decode_result {
    ($response:expr, $request:expr, $p:ident) => {{
        match $response.result.as_ref().ok_or_else(invalid)? {
            $p::content_read_response::Result::Topology(_) => {
                Err(fail(ClientFailureClass::MissingCapability))
            }
            $p::content_read_response::Result::Page(page) => {
                content::$p::decode_page(page, &$request)
                    .map(ContentReadResult::Ready)
                    .map_err(ClientFailure::from)
            }
            $p::content_read_response::Result::ResyncRequired(value) => {
                Ok(ContentReadResult::ResyncRequired {
                    request: $request.stamp(),
                    current: content::$p::decode_base(value.current.as_ref().ok_or_else(invalid)?)
                        .map_err(ClientFailure::from)?,
                })
            }
            $p::content_read_response::Result::Rejected(value) => Ok(ContentReadResult::Rejected {
                request: $request.stamp(),
                reason: read_failure(value.reason)?,
            }),
        }
    }};
}
use decode_result;

fn read_failure(value: i32) -> Result<ContentReadFailure, ClientFailure> {
    Ok(match value {
        1 => ContentReadFailure::StaleAuthority,
        2 => ContentReadFailure::Cancelled,
        3 => ContentReadFailure::Retired,
        4 => ContentReadFailure::Capacity,
        5 => ContentReadFailure::MissingResource,
        6 => ContentReadFailure::InvalidRange,
        7 => ContentReadFailure::Exhausted,
        _ => return Err(invalid()),
    })
}
fn admit_read(
    state: &State,
    server: ServerId,
    stamp: QueryStamp,
) -> Result<OutboundClass, ClientFailure> {
    let pending = state
        .reads
        .get(&(server, stamp.request().get()))
        .ok_or_else(stale)?;
    if !pending.permit.is_current() || pending.request.stamp() != stamp {
        return Err(stale());
    }
    Ok(OutboundClass::Ordinary)
}
fn page_limits() -> Result<PageLimits, ClientFailure> {
    PageLimits::new(256, 16_384, 128 * 1024).map_err(|_| invalid())
}
fn context_payload(
    major: u32,
    request: u64,
    epoch: ServerEpoch,
    context: Context,
) -> Result<Payload, ClientFailure> {
    macro_rules! profile {
        ($p:ident, $variant:ident) => {
            Payload::$variant($p::Message {
                kind: Some($p::message::Kind::ClientContext($p::ClientContext {
                    request,
                    epoch: epoch.get(),
                    connection_generation: context.binding.generation.get(),
                    context_generation: context.generation.get(),
                    focused_pane: context.focused.map(identity::encode_pane),
                    terminal_input_focus: context.input,
                    native_effects_eligible: context.native,
                    ..Default::default()
                })),
            })
        };
    }
    Ok(match major {
        1 => profile!(v1, Major1),
        2 => profile!(v2, Major2),
        _ => return Err(invalid()),
    })
}
fn subscription_payload(
    major: u32,
    subscription: &Subscription,
    cancel: bool,
) -> Result<Payload, ClientFailure> {
    let limits = page_limits()?;
    macro_rules! profile {
        ($p:ident, $variant:ident) => {
            Payload::$variant($p::Message {
                kind: Some($p::message::Kind::PaneSubscription($p::PaneSubscription {
                    fence: Some(identity::encode_fence(subscription.fence)),
                    request: subscription.request,
                    request_generation: subscription.generation,
                    cancel,
                    limits: (!cancel).then_some($p::PageLimits {
                        rows: u32::from(limits.rows()),
                        cells: limits.cells(),
                        bytes: limits.bytes(),
                    }),
                })),
            })
        };
    }
    Ok(match major {
        1 => profile!(v1, Major1),
        2 => profile!(v2, Major2),
        _ => return Err(invalid()),
    })
}
fn read_payload(major: u32, request: &PaneReadRequest) -> Result<Payload, ClientFailure> {
    macro_rules! profile {
        ($p:ident, $variant:ident) => {
            Payload::$variant($p::Message {
                kind: Some(match request {
                    PaneReadRequest::Page(request) => $p::message::Kind::ContentQuery(
                        content::$p::encode_page_request(*request).map_err(ClientFailure::from)?,
                    ),
                    PaneReadRequest::Graphics(request) => $p::message::Kind::ResourceRequest(
                        content::$p::encode_graphics_request(*request),
                    ),
                    PaneReadRequest::Topology(request) => $p::message::Kind::ContentQuery(
                        reads::$p::encode_query(&ReadQuery::Topology(*request)),
                    ),
                    PaneReadRequest::Copy(request) => $p::message::Kind::ContentQuery(
                        reads::$p::encode_query(&ReadQuery::Copy(*request)),
                    ),
                    PaneReadRequest::Find(request) => $p::message::Kind::ContentQuery(
                        reads::$p::encode_query(&ReadQuery::Find(request.clone())),
                    ),
                }),
            })
        };
    }
    Ok(match major {
        1 => profile!(v1, Major1),
        2 => profile!(v2, Major2),
        _ => return Err(invalid()),
    })
}
struct ReadSource {
    receiver: mpsc::Receiver<Result<PaneReadItem, ClientFailure>>,
    owner: std::sync::Weak<NativeProtocol>,
    key: (ServerId, u64),
}
impl PaneReadSource for ReadSource {
    fn next(&mut self) -> ClientFuture<'_, Option<PaneReadItem>> {
        Box::pin(async move {
            tokio::time::timeout(Duration::from_secs(15), self.receiver.recv())
                .await
                .map_err(|_| fail(ClientFailureClass::Unavailable))?
                .transpose()
        })
    }
}
impl Drop for ReadSource {
    fn drop(&mut self) {
        if let Some(owner) = self.owner.upgrade() {
            let pending = owner
                .terminal
                .state
                .lock()
                .ok()
                .and_then(|mut state| state.reads.remove(&self.key));
            if let Some(pending) = pending {
                owner.cancel_read(pending.request);
            }
        }
    }
}
