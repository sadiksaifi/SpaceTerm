//! The client's half of one connection: [`ServerLink`].
//!
//! Callers on any thread send requests, input and acknowledgements. The connection's I/O thread
//! moves frames between the link and its carrier with [`ServerLink::next_frame`] and
//! [`ServerLink::receive`], and judges health with [`ServerLink::check`]. The link delivers pushed
//! messages to sinks on the I/O thread, outside its lock. Sinks must not block; they forward each
//! event to their owner's queue.
use super::Wake;
use super::health::{Check, Death, Liveness};
use crate::pane::{AttachmentClaim, PaneSize, PresentationStyle, effects};
use crate::protocol::frame::{Assembler, encode_client};
use crate::protocol::{
    Acknowledge, Cancel, Channel, Claim, ClientMessage, EffectDone, Fatal, Frame, FrameAck, Input,
    InputEvent, InventoryRevision, Operation, PaneEvent, Ping, Pong, RawFrame, Refusal, Reply,
    ReplyBody, Request, RequestId, ServerMessage, View, limits,
};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

/// Why a link stopped, or why it refused to send. Classifications only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LinkError {
    #[error("the connection closed")]
    Closed,
    #[error("the server stopped responding")]
    Unresponsive,
    #[error("the connection stopped making progress")]
    Stalled,
    #[error("the server closed the connection")]
    Goodbye(Fatal),
    #[error("the server broke the protocol")]
    Violation,
    /// The link stays usable. The caller exceeded the requests in flight.
    #[error("too many requests are in flight")]
    Busy,
    /// The link stays usable. The message exceeded a protocol bound and was not sent.
    #[error("the message exceeds a protocol bound")]
    OutOfBounds,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RequestError {
    #[error("the server refused the request")]
    Refused(Refusal),
    #[error("{0}")]
    Link(LinkError),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Health {
    Live,
    /// Final. The owner reconnects with a new link under ADR 0045.
    Closed(LinkError),
}

pub enum LinkEvent {
    Inventory(Box<InventoryRevision>),
    Health(Health),
}

pub enum ChannelEvent {
    /// Apply it, then call [`ChannelHandle::acknowledge`] before rendering.
    Frame(Box<Frame>),
    Pane(PaneEvent),
}

pub trait LinkSink: Send + Sync {
    fn deliver(&self, event: LinkEvent);
}
impl<F: Fn(LinkEvent) + Send + Sync> LinkSink for F {
    fn deliver(&self, event: LinkEvent) {
        self(event)
    }
}

pub trait ChannelSink: Send + Sync {
    fn deliver(&self, event: ChannelEvent);
}
impl<F: Fn(ChannelEvent) + Send + Sync> ChannelSink for F {
    fn deliver(&self, event: ChannelEvent) {
        self(event)
    }
}

type Callback = Box<dyn FnOnce(Result<ReplyBody, RequestError>) + Send>;

enum Waiter {
    Slot(Arc<ReplySlot>),
    Callback(Callback),
}

#[derive(Default)]
struct ReplySlot {
    value: Mutex<Option<Result<ReplyBody, RequestError>>>,
    ready: Condvar,
}

/// Clones share one connection.
#[derive(Clone)]
pub struct ServerLink {
    shared: Arc<Shared>,
}

struct Shared {
    state: Mutex<State>,
    wake: Arc<dyn Wake>,
    events: Arc<dyn LinkSink>,
}

struct State {
    next_id: u32,
    requests: HashMap<RequestId, Waiter>,
    next_channel: u32,
    channels: HashMap<Channel, ChannelState>,
    l0: VecDeque<Vec<u8>>,
    l0_bytes: usize,
    l1: VecDeque<Vec<u8>>,
    assembler: Assembler,
    liveness: Liveness,
    closed: Option<LinkError>,
}

struct ChannelState {
    sink: Arc<dyn ChannelSink>,
    serial: u32,
}

impl ServerLink {
    pub fn new(events: Arc<dyn LinkSink>, wake: Arc<dyn Wake>, now: Instant) -> Self {
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State {
                    next_id: 0,
                    requests: HashMap::new(),
                    next_channel: 0,
                    channels: HashMap::new(),
                    l0: VecDeque::new(),
                    l0_bytes: 0,
                    l1: VecDeque::new(),
                    assembler: Assembler::default(),
                    liveness: Liveness::new(now),
                    closed: None,
                }),
                wake,
                events,
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.shared.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn health(&self) -> Health {
        match self.lock().closed {
            Some(error) => Health::Closed(error),
            None => Health::Live,
        }
    }

    /// Sends a request whose reply the caller waits for.
    pub fn request(&self, operation: Operation) -> Result<PendingReply, LinkError> {
        let slot = Arc::new(ReplySlot::default());
        let id = self.send_request(operation, Waiter::Slot(Arc::clone(&slot)))?;
        Ok(PendingReply {
            link: self.clone(),
            id,
            slot,
        })
    }

    /// Sends a request whose reply `then` receives on the I/O thread. `then` must not block.
    pub fn request_then(
        &self,
        operation: Operation,
        then: impl FnOnce(Result<ReplyBody, RequestError>) + Send + 'static,
    ) -> Result<RequestId, LinkError> {
        self.send_request(operation, Waiter::Callback(Box::new(then)))
    }

    fn send_request(&self, operation: Operation, waiter: Waiter) -> Result<RequestId, LinkError> {
        let mut state = self.lock();
        state.usable()?;
        if state.requests.len() >= limits::MAX_REQUESTS_IN_FLIGHT {
            return Err(LinkError::Busy);
        }
        let id = loop {
            state.next_id = state.next_id.wrapping_add(1);
            let id = RequestId(state.next_id);
            if !state.requests.contains_key(&id) {
                break id;
            }
        };
        // A claim travels at L0, so its channel's input, views and acknowledgements never
        // overtake it.
        let urgent = matches!(operation, Operation::Claim(_));
        let message = ClientMessage::Request(Box::new(Request { id, operation }));
        state.queue(&message, urgent)?;
        state.requests.insert(id, waiter);
        drop(state);
        self.shared.wake.wake();
        Ok(id)
    }

    /// Opens a channel whose frames and Pane events go to `sink`. Claim it next with
    /// [`ChannelHandle::claim`]. Dropping the handle stops delivery.
    pub fn open_channel(&self, sink: Arc<dyn ChannelSink>) -> Result<ChannelHandle, LinkError> {
        let mut state = self.lock();
        state.usable()?;
        if state.channels.len() >= limits::MAX_CHANNELS {
            return Err(LinkError::Busy);
        }
        state.next_channel = state.next_channel.checked_add(1).ok_or(LinkError::Busy)?;
        let channel = Channel(state.next_channel);
        state
            .channels
            .insert(channel, ChannelState { sink, serial: 0 });
        Ok(ChannelHandle {
            link: self.clone(),
            channel,
        })
    }

    /// Retires a resolved Workspace edit.
    pub fn acknowledge(&self, sequence: u64) -> Result<(), LinkError> {
        self.send(&ClientMessage::Acknowledge(Acknowledge { sequence }), false)
    }

    fn send(&self, message: &ClientMessage, urgent: bool) -> Result<(), LinkError> {
        let mut state = self.lock();
        state.usable()?;
        state.queue(message, urgent)?;
        drop(state);
        self.shared.wake.wake();
        Ok(())
    }

    /// Ends the link, failing every pending request. Idempotent.
    pub fn close(&self) {
        self.fail(LinkError::Closed);
    }

    /// I/O thread: the next whole frame to write. L0 (claims, input, frame acknowledgements,
    /// views, pings) precedes L1 (other requests and messages).
    pub fn next_frame(&self) -> Option<Vec<u8>> {
        let mut state = self.lock();
        if state.closed.is_some() {
            return None;
        }
        if let Some(frame) = state.l0.pop_front() {
            state.l0_bytes -= frame.len();
            return Some(frame);
        }
        state.l1.pop_front()
    }

    /// I/O thread: records whether queued bytes wait on the carrier after a write attempt.
    pub fn wrote(&self, now: Instant, blocked: bool) {
        self.lock().liveness.wrote(now, blocked);
    }

    /// I/O thread: applies one received frame. An error is final and already reported as health.
    pub fn receive(&self, frame: RawFrame, now: Instant) -> Result<(), LinkError> {
        let message = {
            let mut state = self.lock();
            state.usable()?;
            state.liveness.received(now);
            state.assembler.server(frame)
        };
        match message {
            Ok(Some(message)) => self.dispatch(message),
            Ok(None) => Ok(()),
            Err(_) => {
                self.fail(LinkError::Violation);
                Err(LinkError::Violation)
            }
        }
    }

    fn dispatch(&self, message: ServerMessage) -> Result<(), LinkError> {
        match message {
            ServerMessage::Reply(reply) => {
                let Reply { id, result } = *reply;
                let waiter = self.lock().requests.remove(&id);
                let Some(waiter) = waiter else {
                    self.fail(LinkError::Violation);
                    return Err(LinkError::Violation);
                };
                waiter.complete(result.map_err(RequestError::Refused));
            }
            ServerMessage::Frame(frame) => {
                let sink = self.sink(frame.channel);
                if let Some(sink) = sink {
                    sink.deliver(ChannelEvent::Frame(frame));
                }
            }
            ServerMessage::Pane(pane) => {
                if let Some(sink) = self.sink(pane.channel) {
                    sink.deliver(ChannelEvent::Pane(pane.event));
                }
            }
            ServerMessage::Inventory(revision) => {
                self.shared.events.deliver(LinkEvent::Inventory(revision));
            }
            ServerMessage::Ping(Ping { nonce }) => {
                self.send(&ClientMessage::Pong(Pong { nonce }), true)?;
            }
            ServerMessage::Pong(_) => {}
            ServerMessage::Goodbye(fatal) => {
                self.fail(LinkError::Goodbye(fatal));
                return Err(LinkError::Goodbye(fatal));
            }
        }
        Ok(())
    }

    fn sink(&self, channel: Channel) -> Option<Arc<dyn ChannelSink>> {
        self.lock()
            .channels
            .get(&channel)
            .map(|channel| Arc::clone(&channel.sink))
    }

    /// I/O thread: pings or ends the link from traffic, and returns when to check again.
    pub fn check(&self, now: Instant) -> Option<Instant> {
        let check = {
            let mut state = self.lock();
            if state.closed.is_some() {
                return None;
            }
            state.liveness.check(now, None)
        };
        match check {
            Check::Live { next } => Some(next),
            Check::Ping { nonce, next } => {
                self.send(&ClientMessage::Ping(Ping { nonce }), true).ok()?;
                Some(next)
            }
            Check::Dead(death) => {
                self.fail(match death {
                    Death::Silent => LinkError::Unresponsive,
                    Death::Stalled => LinkError::Stalled,
                });
                None
            }
        }
    }

    /// I/O thread: ends the link after a carrier failure. Pending requests fail with `error`;
    /// their operations may or may not have run, and nothing is replayed.
    pub fn fail(&self, error: LinkError) {
        let waiters = {
            let mut state = self.lock();
            if state.closed.is_some() {
                return;
            }
            state.closed = Some(error);
            state.l0.clear();
            state.l1.clear();
            std::mem::take(&mut state.requests)
        };
        for waiter in waiters.into_values() {
            waiter.complete(Err(RequestError::Link(error)));
        }
        self.shared
            .events
            .deliver(LinkEvent::Health(Health::Closed(error)));
        self.shared.wake.wake();
    }
}

impl State {
    fn usable(&self) -> Result<(), LinkError> {
        match self.closed {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn queue(&mut self, message: &ClientMessage, urgent: bool) -> Result<(), LinkError> {
        message.validate().map_err(|_| LinkError::OutOfBounds)?;
        let message = encode_client(message).map_err(|_| LinkError::OutOfBounds)?;
        if urgent {
            if self.l0.len() + message.frames.len() > limits::MAX_L0_FRAMES
                || self.l0_bytes + message.bytes() > limits::MAX_L0_BYTES
            {
                return Err(LinkError::Stalled);
            }
            self.l0_bytes += message.bytes();
            self.l0.extend(message.frames);
        } else {
            self.l1.extend(message.frames);
        }
        Ok(())
    }
}

impl Waiter {
    fn complete(self, result: Result<ReplyBody, RequestError>) {
        match self {
            Self::Slot(slot) => {
                *slot.value.lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
                slot.ready.notify_all();
            }
            Self::Callback(then) => then(result),
        }
    }
}

/// The reply to one request.
pub struct PendingReply {
    link: ServerLink,
    id: RequestId,
    slot: Arc<ReplySlot>,
}

impl PendingReply {
    pub fn id(&self) -> RequestId {
        self.id
    }

    /// Blocks until the reply arrives or the link ends.
    pub fn wait(self) -> Result<ReplyBody, RequestError> {
        let mut value = self.slot.value.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(result) = value.take() {
                return result;
            }
            value = self
                .slot
                .ready
                .wait(value)
                .unwrap_or_else(|e| e.into_inner());
        }
    }

    /// The reply, if it arrived.
    pub fn try_take(&self) -> Option<Result<ReplyBody, RequestError>> {
        self.slot
            .value
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }

    /// Abandons a cancellable read. Its reply still arrives and is dropped.
    pub fn cancel(self) {
        let _ = self
            .link
            .send(&ClientMessage::Cancel(Cancel { id: self.id }), false);
    }
}

/// One claimed or claimable Pane on one link.
pub struct ChannelHandle {
    link: ServerLink,
    channel: Channel,
}

impl ChannelHandle {
    pub fn channel(&self) -> Channel {
        self.channel
    }

    /// Claims a Pane for this channel. Frames may arrive before the reply.
    pub fn claim(
        &self,
        attachment: crate::attachments::Attachment,
        pane: crate::hierarchy::Id,
        claim: AttachmentClaim,
        size: PaneSize,
        style: Option<PresentationStyle>,
    ) -> Result<PendingReply, LinkError> {
        self.link.request(Operation::Claim(Claim {
            channel: self.channel,
            attachment,
            pane,
            claim,
            size,
            style,
        }))
    }

    /// Sends one Input message at L0 and returns its serial. Input is fire-and-forget: frames
    /// report the serials they reflect, and `PaneEvent::Input` reports a discard.
    pub fn input(&self, generation: u64, events: Vec<InputEvent>) -> Result<u32, LinkError> {
        let mut state = self.link.lock();
        state.usable()?;
        let channel = state
            .channels
            .get_mut(&self.channel)
            .ok_or(LinkError::Closed)?;
        let serial = channel
            .serial
            .checked_add(1)
            .ok_or(LinkError::OutOfBounds)?;
        let message = ClientMessage::Input(Input {
            channel: self.channel,
            serial,
            generation,
            events,
        });
        state.queue(&message, true)?;
        if let Some(channel) = state.channels.get_mut(&self.channel) {
            channel.serial = serial;
        }
        drop(state);
        self.link.shared.wake.wake();
        Ok(serial)
    }

    pub fn acknowledge(&self, frame: u64) -> Result<(), LinkError> {
        self.link.send(
            &ClientMessage::FrameAck(FrameAck {
                channel: self.channel,
                frame,
            }),
            true,
        )
    }

    pub fn view(&self, visible: bool, max_hz: u8) -> Result<(), LinkError> {
        self.link.send(
            &ClientMessage::View(View {
                channel: self.channel,
                visible,
                max_hz,
            }),
            true,
        )
    }

    pub fn effect_done(
        &self,
        generation: u64,
        effect: u64,
        completion: effects::Completion,
    ) -> Result<(), LinkError> {
        self.link.send(
            &ClientMessage::EffectDone(EffectDone {
                channel: self.channel,
                generation,
                effect,
                completion,
            }),
            false,
        )
    }

    pub fn request(&self, operation: Operation) -> Result<PendingReply, LinkError> {
        self.link.request(operation)
    }
}

impl Drop for ChannelHandle {
    fn drop(&mut self) {
        self.link.lock().channels.remove(&self.channel);
    }
}
