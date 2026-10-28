//! The client's half of one connection: [`ServerLink`].
//!
//! Callers on any thread send requests, input and acknowledgements. The connection's I/O thread
//! moves frames between the link and its carrier with [`ServerLink::next_frame`] and
//! [`ServerLink::receive`], and judges health with [`ServerLink::check`]. The link delivers pushed
//! messages to sinks on the I/O thread, outside its lock. Sinks must not block; they forward each
//! event to their owner's queue.
//!
//! Every request, input, frame acknowledgement, view and ping travels at L0 in one FIFO, so a
//! claim never overtakes an earlier request, and a channel's input never overtakes its claim.
//! `EffectDone`, `Acknowledge` and `Cancel` travel at L1.
use super::Wake;
use super::carrier::Priority;
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
    /// The link stays usable. The caller exceeded the requests in flight, the open channels, or
    /// the queued input.
    #[error("the connection is busy")]
    Busy,
    /// The link stays usable. The message exceeded a protocol bound and was not sent.
    #[error("the message exceeds a protocol bound")]
    OutOfBounds,
    /// The link stays usable. The channel was not claimed, was claimed twice, or has ended.
    #[error("the channel is not claimed")]
    Unclaimed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RequestError {
    #[error("the server refused the request")]
    Refused(Refusal),
    #[error("{0}")]
    Link(LinkError),
}

impl From<LinkError> for crate::ServerError {
    fn from(error: LinkError) -> Self {
        match error {
            LinkError::Goodbye(Fatal::Stopping) => Self::NotRunning,
            LinkError::Goodbye(Fatal::Revoked) => Self::AuthenticationRefused,
            LinkError::Goodbye(Fatal::Incompatible) => Self::IncompatibleProtocol,
            LinkError::Goodbye(Fatal::Capacity) => Self::Capacity,
            // The link stays usable; a brief stall is worth a retry.
            LinkError::Busy => Self::Protocol,
            LinkError::Unclaimed => Self::StaleControl,
            LinkError::OutOfBounds => Self::Unsupported,
            LinkError::Closed
            | LinkError::Unresponsive
            | LinkError::Stalled
            | LinkError::Violation
            | LinkError::Goodbye(_) => Self::Protocol,
        }
    }
}

impl From<RequestError> for crate::ServerError {
    fn from(error: RequestError) -> Self {
        match error {
            RequestError::Refused(refusal) => refusal.into(),
            RequestError::Link(error) => error.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Health {
    Live,
    /// Final. The owner reconnects with a new link under ADR 0045.
    Closed(LinkError),
}

pub enum LinkEvent {
    Inventory(Arc<InventoryRevision>),
    Health(Health),
}

pub enum ChannelEvent {
    /// Apply it, then call [`ChannelHandle::acknowledge`] before rendering.
    Frame(Box<Frame>),
    /// An event for which [`PaneEvent::last`] holds is the channel's last. Every frame before it
    /// is still applied and acknowledged.
    Pane(PaneEvent),
    /// The link ended. Final; the owner reconnects with a new link under ADR 0045.
    Closed(LinkError),
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
    /// Notified when an inventory revision arrives or the link ends.
    revisions: Condvar,
    wake: Arc<dyn Wake>,
    events: Arc<dyn LinkSink>,
}

impl Drop for Shared {
    /// The last handle is gone: the I/O thread wakes, finds no link and closes the carrier.
    fn drop(&mut self) {
        self.wake.wake();
    }
}

/// The I/O thread's reference. It keeps no link alive, so dropping every [`ServerLink`] ends the
/// connection.
pub(crate) struct WeakLink(std::sync::Weak<Shared>);

impl WeakLink {
    pub(crate) fn upgrade(&self) -> Option<ServerLink> {
        self.0.upgrade().map(|shared| ServerLink { shared })
    }
}

struct State {
    next_id: u32,
    requests: HashMap<RequestId, Waiter>,
    next_channel: u32,
    channels: HashMap<Channel, ChannelState>,
    /// Channels whose handle dropped while their `Release` waits for request capacity.
    releases: VecDeque<Channel>,
    l0: VecDeque<Queued>,
    /// Bytes of queued L0 frames other than requests, which the in-flight limit bounds.
    l0_bytes: usize,
    l1: VecDeque<Vec<u8>>,
    assembler: Assembler,
    liveness: Liveness,
    closed: Option<LinkError>,
    /// The newest inventory revision, and how many this link received.
    inventory: Option<(u64, Arc<InventoryRevision>)>,
}

struct Queued {
    frame: Vec<u8>,
    counted: bool,
}

struct ChannelState {
    sink: Arc<dyn ChannelSink>,
    serial: u32,
    claimed: bool,
    /// The server sent the channel's last event, so dropping the handle sends no `Release`.
    ended: bool,
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
                    releases: VecDeque::new(),
                    l0: VecDeque::new(),
                    l0_bytes: 0,
                    l1: VecDeque::new(),
                    assembler: Assembler::default(),
                    liveness: Liveness::new(now),
                    closed: None,
                    inventory: None,
                }),
                revisions: Condvar::new(),
                wake,
                events,
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.shared.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn downgrade(&self) -> WeakLink {
        WeakLink(Arc::downgrade(&self.shared))
    }

    fn wake(&self) {
        self.shared.wake.wake();
    }

    pub fn health(&self) -> Health {
        match self.lock().closed {
            Some(error) => Health::Closed(error),
            None => Health::Live,
        }
    }

    /// The newest Workspace Inventory revision this link received, with how many it received.
    /// The server pushes one when it admits the link, and each one before the reply to the edit
    /// it holds.
    pub fn inventory(&self) -> Option<(u64, Arc<InventoryRevision>)> {
        self.lock().inventory.clone()
    }

    /// Waits until this link received more than `seen` revisions, and returns the newest. Fails
    /// once the link ends. Never call it on the I/O thread.
    pub fn next_inventory(&self, seen: u64) -> Result<(u64, Arc<InventoryRevision>), LinkError> {
        let mut state = self.lock();
        loop {
            if let Some((count, revision)) = &state.inventory
                && *count > seen
            {
                return Ok((*count, Arc::clone(revision)));
            }
            if let Some(error) = state.closed {
                return Err(error);
            }
            state = self
                .shared
                .revisions
                .wait(state)
                .unwrap_or_else(|e| e.into_inner());
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
        // Releases of dropped channels go first, so they never wait behind new requests.
        state.send_releases();
        let id = state.send_request(operation, waiter)?;
        drop(state);
        self.wake();
        Ok(id)
    }

    /// Opens a channel whose frames and Pane events go to `sink`. Claim it next with
    /// [`ChannelHandle::claim`]. Dropping the handle sends `Release` unless the channel ended.
    pub fn open_channel(&self, sink: Arc<dyn ChannelSink>) -> Result<ChannelHandle, LinkError> {
        let mut state = self.lock();
        state.usable()?;
        if state.channels.len() + state.releases.len() >= limits::MAX_CHANNELS {
            return Err(LinkError::Busy);
        }
        state.next_channel = state.next_channel.checked_add(1).ok_or(LinkError::Busy)?;
        let channel = Channel(state.next_channel);
        state.channels.insert(
            channel,
            ChannelState {
                sink,
                serial: 0,
                claimed: false,
                ended: false,
            },
        );
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
        self.wake();
        Ok(())
    }

    /// Ends the link, failing every pending request. Idempotent.
    pub fn close(&self) {
        self.fail(LinkError::Closed);
    }

    /// I/O thread: the next whole frame to write and its level.
    pub fn next_frame(&self) -> Option<(Vec<u8>, Priority)> {
        let mut state = self.lock();
        if state.closed.is_some() {
            return None;
        }
        if let Some(queued) = state.l0.pop_front() {
            if queued.counted {
                state.l0_bytes -= queued.frame.len();
            }
            return Some((queued.frame, Priority::L0));
        }
        state.l1.pop_front().map(|frame| (frame, Priority::L1))
    }

    /// I/O thread: records a write attempt; see [`Liveness::wrote`].
    pub fn wrote(&self, now: Instant, progressed: bool, pending: bool) {
        self.lock().liveness.wrote(now, progressed, pending);
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
                let waiter = {
                    let mut state = self.lock();
                    let waiter = state.requests.remove(&id);
                    if state.send_releases() {
                        drop(state);
                        self.wake();
                    }
                    waiter
                };
                let Some(waiter) = waiter else {
                    self.fail(LinkError::Violation);
                    return Err(LinkError::Violation);
                };
                waiter.complete(result.map_err(RequestError::Refused));
            }
            ServerMessage::Frame(frame) => match self.sink(frame.channel, false) {
                Some(sink) => sink.deliver(ChannelEvent::Frame(frame)),
                // Nobody holds the channel any more. Acknowledging keeps its credit from stalling
                // the connection until the server processes the `Release`.
                // A full L0 drops this acknowledgement; the server frees the credit when it
                // processes the `Release`. Either way the link stays usable.
                None => {
                    let _ = self.send(
                        &ClientMessage::FrameAck(FrameAck {
                            channel: frame.channel,
                            frame: frame.frame,
                        }),
                        true,
                    );
                }
            },
            ServerMessage::Pane(pane) => {
                let last = pane.event.last();
                if let Some(sink) = self.sink(pane.channel, last) {
                    sink.deliver(ChannelEvent::Pane(pane.event));
                }
            }
            ServerMessage::Inventory(revision) => {
                let revision: Arc<InventoryRevision> = Arc::from(revision);
                {
                    let mut state = self.lock();
                    let count = state.inventory.as_ref().map_or(0, |(count, _)| *count) + 1;
                    state.inventory = Some((count, Arc::clone(&revision)));
                }
                self.shared.revisions.notify_all();
                self.shared.events.deliver(LinkEvent::Inventory(revision));
            }
            // A full L0 drops the pong. The traffic that filled it renews the server's liveness.
            ServerMessage::Ping(Ping { nonce }) => {
                let _ = self.send(&ClientMessage::Pong(Pong { nonce }), true);
            }
            ServerMessage::Pong(_) => {}
            ServerMessage::Goodbye(fatal) => {
                self.fail(LinkError::Goodbye(fatal));
                return Err(LinkError::Goodbye(fatal));
            }
        }
        Ok(())
    }

    fn sink(&self, channel: Channel, last: bool) -> Option<Arc<dyn ChannelSink>> {
        let mut state = self.lock();
        let channel = state.channels.get_mut(&channel)?;
        channel.ended |= last;
        Some(Arc::clone(&channel.sink))
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
                match self.send(&ClientMessage::Ping(Ping { nonce }), true) {
                    Ok(()) | Err(LinkError::Busy) => Some(next),
                    Err(_) => None,
                }
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

    /// I/O thread: whether the server left a ping unanswered long enough that the network path
    /// may be gone. True once per ping.
    pub fn suspect(&self, now: Instant) -> bool {
        self.lock().liveness.suspect(now)
    }

    /// I/O thread: ends the link after a carrier failure. Pending requests fail with `error`;
    /// their operations may or may not have run, and nothing is replayed.
    pub fn fail(&self, error: LinkError) {
        let (waiters, channels) = {
            let mut state = self.lock();
            if state.closed.is_some() {
                return;
            }
            state.closed = Some(error);
            state.l0.clear();
            state.l1.clear();
            state.releases.clear();
            let channels: Vec<_> = state
                .channels
                .values_mut()
                .filter(|channel| !channel.ended)
                .map(|channel| {
                    channel.ended = true;
                    Arc::clone(&channel.sink)
                })
                .collect();
            (std::mem::take(&mut state.requests), channels)
        };
        for waiter in waiters.into_values() {
            waiter.complete(Err(RequestError::Link(error)));
        }
        self.shared.revisions.notify_all();
        for sink in channels {
            sink.deliver(ChannelEvent::Closed(error));
        }
        self.shared
            .events
            .deliver(LinkEvent::Health(Health::Closed(error)));
        self.wake();
    }

    fn release(&self, channel: Channel) {
        let mut state = self.lock();
        let Some(dropped) = state.channels.remove(&channel) else {
            return;
        };
        if !dropped.claimed || dropped.ended || state.closed.is_some() {
            return;
        }
        state.releases.push_back(channel);
        if state.send_releases() {
            drop(state);
            self.wake();
        }
    }
}

impl State {
    fn usable(&self) -> Result<(), LinkError> {
        match self.closed {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn send_request(
        &mut self,
        operation: Operation,
        waiter: Waiter,
    ) -> Result<RequestId, LinkError> {
        if self.requests.len() >= limits::MAX_REQUESTS_IN_FLIGHT {
            return Err(LinkError::Busy);
        }
        let id = loop {
            self.next_id = self.next_id.wrapping_add(1);
            let id = RequestId(self.next_id);
            if !self.requests.contains_key(&id) {
                break id;
            }
        };
        let message = ClientMessage::Request(Box::new(Request { id, operation }));
        message.validate().map_err(|_| LinkError::OutOfBounds)?;
        let wire = encode_client(&message).map_err(|_| LinkError::OutOfBounds)?;
        self.l0.extend(wire.frames.into_iter().map(|frame| Queued {
            frame,
            counted: false,
        }));
        self.requests.insert(id, waiter);
        Ok(id)
    }

    /// Sends queued releases while request capacity allows. Returns whether any was sent.
    fn send_releases(&mut self) -> bool {
        let mut sent = false;
        while self.requests.len() < limits::MAX_REQUESTS_IN_FLIGHT
            && let Some(channel) = self.releases.pop_front()
        {
            let ignore = Waiter::Callback(Box::new(|_| {}));
            sent |= self
                .send_request(Operation::Release { channel }, ignore)
                .is_ok();
        }
        sent
    }

    /// Queues one message, or none on error.
    fn queue(&mut self, message: &ClientMessage, urgent: bool) -> Result<(), LinkError> {
        self.queue_all(std::slice::from_ref(message), urgent)
    }

    /// Queues every message or none, so a paste is never partly sent.
    fn queue_all(&mut self, messages: &[ClientMessage], urgent: bool) -> Result<(), LinkError> {
        let mut frames = Vec::new();
        for message in messages {
            message.validate().map_err(|_| LinkError::OutOfBounds)?;
            frames.extend(
                encode_client(message)
                    .map_err(|_| LinkError::OutOfBounds)?
                    .frames,
            );
        }
        if !urgent {
            self.l1.extend(frames);
            return Ok(());
        }
        let bytes: usize = frames.iter().map(Vec::len).sum();
        if self.l0.len() + frames.len() > limits::MAX_L0_FRAMES + limits::MAX_REQUESTS_IN_FLIGHT
            || self.l0_bytes + bytes > limits::MAX_CLIENT_L0_BYTES
        {
            return Err(LinkError::Busy);
        }
        self.l0_bytes += bytes;
        self.l0.extend(frames.into_iter().map(|frame| Queued {
            frame,
            counted: true,
        }));
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

    /// Blocks until the reply arrives or the link ends. Never call it on the I/O thread.
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

    /// Waits up to `timeout` for the reply. `None` means it has not arrived.
    pub fn wait_for(
        &self,
        timeout: std::time::Duration,
    ) -> Option<Result<ReplyBody, RequestError>> {
        let deadline = Instant::now() + timeout;
        let mut value = self.slot.value.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(result) = value.take() {
                return Some(result);
            }
            let remaining = deadline.checked_duration_since(Instant::now())?;
            value = self
                .slot
                .ready
                .wait_timeout(value, remaining)
                .unwrap_or_else(|e| e.into_inner())
                .0;
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

/// One claimable Pane on one link.
pub struct ChannelHandle {
    link: ServerLink,
    channel: Channel,
}

impl ChannelHandle {
    pub fn channel(&self) -> Channel {
        self.channel
    }

    /// Claims a Pane for this channel, once. Frames may arrive before the reply. A reply other
    /// than `ClaimReply::Attached` leaves the channel unbound; drop the handle.
    pub fn claim(
        &self,
        attachment: crate::attachments::Attachment,
        pane: crate::hierarchy::Id,
        claim: AttachmentClaim,
        size: PaneSize,
        style: Option<PresentationStyle>,
    ) -> Result<PendingReply, LinkError> {
        let slot = Arc::new(ReplySlot::default());
        let mut state = self.link.lock();
        state.usable()?;
        let channel = state
            .channels
            .get_mut(&self.channel)
            .ok_or(LinkError::Unclaimed)?;
        if channel.claimed {
            return Err(LinkError::Unclaimed);
        }
        state.send_releases();
        let operation = Operation::Claim(Claim {
            channel: self.channel,
            attachment,
            pane,
            claim,
            size,
            style,
        });
        let id = state.send_request(operation, Waiter::Slot(Arc::clone(&slot)))?;
        if let Some(channel) = state.channels.get_mut(&self.channel) {
            channel.claimed = true;
        }
        drop(state);
        self.link.wake();
        Ok(PendingReply {
            link: self.link.clone(),
            id,
            slot,
        })
    }

    /// Queues messages on this claimed, live channel. `build` receives the next serial.
    fn send_channel(
        &self,
        urgent: bool,
        build: impl FnOnce(u32) -> Result<(Vec<ClientMessage>, u32), LinkError>,
    ) -> Result<u32, LinkError> {
        let mut state = self.link.lock();
        state.usable()?;
        let channel = state
            .channels
            .get(&self.channel)
            .ok_or(LinkError::Unclaimed)?;
        if !channel.claimed || channel.ended {
            return Err(LinkError::Unclaimed);
        }
        let (messages, serial) = build(channel.serial)?;
        state.queue_all(&messages, urgent)?;
        if let Some(channel) = state.channels.get_mut(&self.channel) {
            channel.serial = serial;
        }
        drop(state);
        self.link.wake();
        Ok(serial)
    }

    /// Sends one Input message at L0 and returns its serial. Input is fire-and-forget: frames
    /// report the serials they reflect, and `PaneEvent::Input` reports a discard. Use
    /// [`ChannelHandle::paste`] for paste text.
    pub fn input(&self, generation: u64, events: Vec<InputEvent>) -> Result<u32, LinkError> {
        let channel = self.channel;
        self.send_channel(true, move |last| {
            let serial = last.checked_add(1).ok_or(LinkError::OutOfBounds)?;
            let message = ClientMessage::Input(Input {
                channel,
                serial,
                generation,
                events,
            });
            Ok((vec![message], serial))
        })
    }

    /// Sends a whole paste of at most [`limits::MAX_PASTE_BYTES`] as consecutive Input messages,
    /// all or none, and returns the serial of the last one, which its outcome names.
    pub fn paste(&self, generation: u64, text: &str) -> Result<u32, LinkError> {
        if text.len() > limits::MAX_PASTE_BYTES {
            return Err(LinkError::OutOfBounds);
        }
        let chunks = chunks(text, limits::MAX_PASTE_CHUNK_BYTES);
        let channel = self.channel;
        self.send_channel(true, move |mut serial| {
            let count = chunks.len();
            let mut messages = Vec::with_capacity(count);
            for (index, chunk) in chunks.into_iter().enumerate() {
                serial = serial.checked_add(1).ok_or(LinkError::OutOfBounds)?;
                messages.push(ClientMessage::Input(Input {
                    channel,
                    serial,
                    generation,
                    events: vec![InputEvent::Paste {
                        text: chunk.to_owned(),
                        last: index + 1 == count,
                    }],
                }));
            }
            Ok((messages, serial))
        })
    }

    fn send_simple(&self, message: ClientMessage, urgent: bool) -> Result<(), LinkError> {
        self.send_channel(urgent, |serial| Ok((vec![message], serial)))
            .map(|_| ())
    }

    pub fn acknowledge(&self, frame: u64) -> Result<(), LinkError> {
        self.send_simple(
            ClientMessage::FrameAck(FrameAck {
                channel: self.channel,
                frame,
            }),
            true,
        )
    }

    pub fn view(&self, visible: bool, max_hz: u8) -> Result<(), LinkError> {
        self.send_simple(
            ClientMessage::View(View {
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
        self.send_simple(
            ClientMessage::EffectDone(EffectDone {
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

impl ChannelHandle {
    /// Forgets the channel here without releasing its Pane, so the claim's control remains, as
    /// for a dismissed presentation. The server keeps the binding until a later claim of the same
    /// Pane on this connection replaces it or the connection ends. Frames that still arrive are
    /// acknowledged by the link.
    pub fn detach(self) {
        // Dropping finds no channel to release.
        self.link.lock().channels.remove(&self.channel);
    }
}

impl Drop for ChannelHandle {
    /// Sends `Release` for a claimed channel that has not ended, so the server frees its slot.
    fn drop(&mut self) {
        self.link.release(self.channel);
    }
}

/// Splits `text` at character boundaries into pieces of at most `limit` bytes. An empty paste is
/// one empty piece, which the server rejects as `Empty`.
fn chunks(text: &str, limit: usize) -> Vec<&str> {
    let mut pieces = Vec::new();
    let mut rest = text;
    loop {
        if rest.len() <= limit {
            pieces.push(rest);
            return pieces;
        }
        let mut end = limit;
        while !rest.is_char_boundary(end) {
            end -= 1;
        }
        let (piece, tail) = rest.split_at(end);
        pieces.push(piece);
        rest = tail;
    }
}
