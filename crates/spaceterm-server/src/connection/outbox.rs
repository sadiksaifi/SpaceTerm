//! A server connection's outgoing messages, in priority order.
//!
//! | Level | Carries |
//! |---|---|
//! | L0 | Ping, Pong, Goodbye |
//! | L1 | small replies, Pane events, frames and inventory, in publication order |
//! | L2 | bulk replies, in frames of at most [`limits::BULK_CHUNK_BYTES`] |
//!
//! The I/O thread writes one frame per [`Outbox::next_frame`] call, so L0 interleaves between the
//! frames of a large L1 or L2 message. Inventory and each channel's presentation are latest-state
//! slots: publishing again replaces a state that is not yet written but keeps its place in L1.
//! Each channel has one frame of credit, returned by its acknowledgement.
//!
//! Replies, Pane events and inventory are encoded when they are queued, on the producer's thread,
//! so the producer receives any encoding error. Presentation is encoded by
//! [`Presentation::encode`] on the I/O thread, outside the `PaneService` lock and this outbox's
//! lock.
use super::Wake;
use super::carrier::Priority;
use crate::protocol::frame::{EncodeError, encode_server};
use crate::protocol::screen::ScreenUpdate;
use crate::protocol::{
    Channel, Fatal, Frame, InventoryRevision, PaneEvent, PaneMessage, Ping, Pong, Refusal, Reply,
    ReplyBody, RequestId, ServerMessage, limits,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// A Pane's screen state captured under the `PaneService` lock and encoded later outside it.
/// It owns its snapshot, for example through an `Arc`, and never borrows service state.
pub trait Presentation: Send {
    /// Encodes this state against `ticket.base`, or as a keyframe when `base` is `None`.
    /// `None` means the state equals the base: no frame is written, credit stays available, and
    /// the encoder must not advance its baseline.
    fn encode(self: Box<Self>, ticket: FrameTicket) -> Option<Presented>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameTicket {
    pub channel: Channel,
    pub frame: u64,
    /// The last acknowledged frame, which the client still holds. `None` requires a keyframe.
    pub base: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct Presented {
    /// The highest input serial written to the PTY before the state was captured.
    pub input: u32,
    pub update: ScreenUpdate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum OutboxError {
    /// A queue passed its bound. The outbox already queued `Goodbye(Stalled)` and closed itself.
    #[error("the connection stalled")]
    Stalled,
    /// The connection has too many channels open.
    #[error("the connection reached capacity")]
    Capacity,
    /// The peer broke an ordering rule, such as acknowledging a frame it was not sent.
    #[error("the peer broke the protocol")]
    Violation,
    /// The connection is closing. Nothing more is queued except what already was.
    #[error("the connection is closing")]
    Closed,
    /// This server could not encode its own message. Nothing was queued, and a reply id stays
    /// owed, so the producer can answer with a refusal instead.
    #[error("the message could not be encoded")]
    Encode(EncodeError),
    /// The server misused the outbox, such as replying to an id it does not owe.
    #[error("the server misused the connection")]
    Misuse,
}

impl From<OutboxError> for Fatal {
    fn from(error: OutboxError) -> Self {
        match error {
            OutboxError::Stalled => Self::Stalled,
            OutboxError::Capacity => Self::Capacity,
            OutboxError::Violation => Self::ProtocolViolation,
            OutboxError::Closed => Self::Stopping,
            OutboxError::Encode(_) | OutboxError::Misuse => Self::Internal,
        }
    }
}

/// Clones share one connection's queues. Every method is safe to call from any thread.
#[derive(Clone)]
pub struct Outbox {
    shared: Arc<Shared>,
}

struct Shared {
    state: Mutex<State>,
    wake: Arc<dyn Wake>,
}

type Frames = Vec<Vec<u8>>;

#[derive(Default)]
struct State {
    l0: VecDeque<Vec<u8>>,
    l0_bytes: usize,
    l1: VecDeque<Entry>,
    l1_rest: VecDeque<Vec<u8>>,
    l2: VecDeque<Frames>,
    l2_rest: VecDeque<Vec<u8>>,
    inventory: Option<Frames>,
    inventory_queued: bool,
    channels: BTreeMap<Channel, Slot>,
    /// Channels closed since the connection last asked, for its serial bookkeeping.
    closed: Vec<Channel>,
    requests: BTreeSet<RequestId>,
    closing: bool,
}

enum Entry {
    Message(Frames),
    Inventory,
    Frame(Channel),
}

struct Slot {
    pending: Option<Box<dyn Presentation>>,
    queued: bool,
    visible: bool,
    next: u64,
    /// The frame awaiting acknowledgement, and when its first wire frame was written.
    in_flight: Option<(u64, Option<Instant>)>,
    acknowledged: Option<u64>,
    keyframe: bool,
    /// Set by [`Outbox::end`]: written after the final frame, then the slot is removed.
    end: Option<Frames>,
}

enum Taken {
    Frame(Vec<u8>, Priority),
    Present(FrameTicket, Box<dyn Presentation>, Option<Frames>),
    Skip,
}

impl Outbox {
    pub fn new(wake: Arc<dyn Wake>) -> Self {
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State::default()),
                wake,
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.shared.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn with<T>(
        &self,
        change: impl FnOnce(&mut State) -> Result<T, OutboxError>,
    ) -> Result<T, OutboxError> {
        let result = change(&mut self.lock());
        if matches!(result, Ok(_) | Err(OutboxError::Stalled)) {
            self.shared.wake.wake();
        }
        result
    }

    /// Registers a request id from the peer. A duplicate in-flight id, or more than
    /// [`limits::MAX_REQUESTS_IN_FLIGHT`], is a violation.
    pub fn begin(&self, id: RequestId) -> Result<(), OutboxError> {
        let mut state = self.lock();
        if state.requests.len() >= limits::MAX_REQUESTS_IN_FLIGHT || !state.requests.insert(id) {
            return Err(OutboxError::Violation);
        }
        Ok(())
    }

    /// Queues the one reply to `id`, bulk bodies at L2. On `Encode`, nothing is queued and the
    /// reply stays owed; prefer [`Outbox::reply_or_refuse`].
    pub fn reply(
        &self,
        id: RequestId,
        result: Result<ReplyBody, Refusal>,
    ) -> Result<(), OutboxError> {
        let bulk = matches!(&result, Ok(body) if body.bulk());
        let frames = encode(&ServerMessage::Reply(Box::new(Reply { id, result })))?;
        self.with(|state| {
            state.open()?;
            if !state.requests.contains(&id) {
                return Err(OutboxError::Misuse);
            }
            if bulk {
                if state.l2.len() >= limits::MAX_L2_MESSAGES {
                    return Err(state.overflow());
                }
                state.l2.push_back(frames);
            } else {
                state.push_l1(Entry::Message(frames))?;
            }
            state.requests.remove(&id);
            Ok(())
        })
    }

    /// Queues `result`, or a refusal if `result` cannot be encoded: `Capacity` for a body over its
    /// bound, otherwise `Failed`. The reply id is never lost.
    pub fn reply_or_refuse(
        &self,
        id: RequestId,
        result: Result<ReplyBody, Refusal>,
    ) -> Result<(), OutboxError> {
        match self.reply(id, result) {
            Err(OutboxError::Encode(error)) => self.reply(
                id,
                Err(match error {
                    EncodeError::TooLarge => Refusal::Capacity,
                    EncodeError::Unencodable => Refusal::Failed,
                }),
            ),
            result => result,
        }
    }

    /// Queues a Pane event that leaves the channel open. Use [`Outbox::end`] for `Ended`,
    /// `SessionEnded`, `Failed` and `Takeover`.
    pub fn pane(&self, channel: Channel, event: PaneEvent) -> Result<(), OutboxError> {
        let frames = encode(&ServerMessage::Pane(PaneMessage { channel, event }))?;
        self.with(|state| state.push_l1(Entry::Message(frames)))
    }

    /// Replaces any inventory revision not yet written. A reply queued after this call is
    /// written after the revision, so a deferred outcome never precedes the revision holding it.
    pub fn inventory(&self, revision: InventoryRevision) -> Result<(), OutboxError> {
        let frames = encode(&ServerMessage::Inventory(Box::new(revision)))?;
        self.with(|state| {
            state.open()?;
            if !state.inventory_queued {
                state.push_l1(Entry::Inventory)?;
                state.inventory_queued = true;
            }
            state.inventory = Some(frames);
            Ok(())
        })
    }

    pub fn ping(&self, nonce: u64) -> Result<(), OutboxError> {
        self.with(|state| state.push_l0(&ServerMessage::Ping(Ping { nonce })))
    }

    pub fn pong(&self, nonce: u64) -> Result<(), OutboxError> {
        self.with(|state| state.push_l0(&ServerMessage::Pong(Pong { nonce })))
    }

    /// Queues the final message. Afterwards only L0 frames already queued and this one are
    /// written, and producers receive `Closed`.
    pub fn goodbye(&self, fatal: Fatal) -> Result<(), OutboxError> {
        let frames = encode(&ServerMessage::Goodbye(fatal))?;
        self.with(|state| {
            state.open()?;
            state.l0.extend(frames);
            state.closing = true;
            Ok(())
        })
    }

    /// Whether a request this connection began still waits for its reply, for example from the
    /// sync worker.
    pub fn owes_replies(&self) -> bool {
        !self.lock().requests.is_empty()
    }

    /// Whether `goodbye` was queued, a queue passed its bound, or the connection ended. The
    /// connection stops dispatching once it is.
    pub fn is_closing(&self) -> bool {
        self.lock().closing
    }

    /// Records that the connection's I/O thread exited. Producers receive `Closed` from now on,
    /// and sync jobs of this connection skip their work.
    pub fn disconnected(&self) {
        self.lock().closing = true;
    }

    /// Starts presenting `channel`, visible and owing a keyframe.
    pub fn open(&self, channel: Channel) -> Result<(), OutboxError> {
        self.with(|state| {
            if state.channels.len() >= limits::MAX_CHANNELS {
                return Err(OutboxError::Capacity);
            }
            if state.channels.contains_key(&channel) {
                return Err(OutboxError::Misuse);
            }
            state.channels.insert(
                channel,
                Slot {
                    pending: None,
                    queued: false,
                    visible: true,
                    next: 1,
                    in_flight: None,
                    acknowledged: None,
                    keyframe: true,
                    end: None,
                },
            );
            Ok(())
        })
    }

    /// Forgets `channel` at once, dropping its pending state, for a client `Release` or `Close`.
    /// Also call it when a claim ends without binding the channel, so the connection forgets it.
    pub fn close(&self, channel: Channel) {
        let mut state = self.lock();
        state.channels.remove(&channel);
        state.closed.push(channel);
    }

    /// Ends `channel` from the server side with `Ended`, `SessionEnded`, `Failed` or `Takeover`.
    /// A pending final state is written first, after the in-flight frame's acknowledgement if one
    /// is in flight; then `event`; then the channel is forgotten. Later presentations are ignored.
    pub fn end(&self, channel: Channel, event: PaneEvent) -> Result<(), OutboxError> {
        let frames = encode(&ServerMessage::Pane(PaneMessage { channel, event }))?;
        self.with(|state| {
            state.open()?;
            let Some(slot) = state.channels.get_mut(&channel) else {
                state.closed.push(channel);
                return state.push_l1(Entry::Message(frames));
            };
            if slot.end.is_some() {
                return Ok(());
            }
            if slot.pending.is_none() && slot.in_flight.is_none() {
                state.channels.remove(&channel);
                state.closed.push(channel);
                return state.push_l1(Entry::Message(frames));
            }
            slot.end = Some(frames);
            Ok(())
        })
    }

    /// Drains the channels closed since the last call.
    pub fn take_closed(&self) -> Vec<Channel> {
        std::mem::take(&mut self.lock().closed)
    }

    /// A hidden channel drops its pending state and receives no frames. Revealing keeps the
    /// acknowledged baseline; the publisher presents the current state again.
    pub fn view(&self, channel: Channel, visible: bool) {
        let mut state = self.lock();
        let mut ended = None;
        if let Some(slot) = state.channels.get_mut(&channel) {
            slot.visible = visible;
            if !visible {
                slot.pending = None;
                if slot.in_flight.is_none() {
                    ended = slot.end.take();
                }
            }
        }
        if let Some(frames) = ended {
            state.channels.remove(&channel);
            state.closed.push(channel);
            // A full L1 closes the outbox with `Goodbye(Stalled)`, so the event is not lost
            // silently: the whole connection ends.
            let _ = state.push_l1(Entry::Message(frames));
            drop(state);
            self.shared.wake.wake();
        }
    }

    /// Replaces the channel's pending state. Ignored for a hidden, ending or closed channel.
    pub fn present(
        &self,
        channel: Channel,
        presentation: Box<dyn Presentation>,
    ) -> Result<(), OutboxError> {
        self.with(|state| {
            state.open()?;
            let Some(slot) = state.channels.get_mut(&channel) else {
                return Ok(());
            };
            if !slot.visible || slot.end.is_some() {
                return Ok(());
            }
            slot.pending = Some(presentation);
            if !slot.queued && slot.in_flight.is_none() {
                slot.queued = true;
                state.push_l1(Entry::Frame(channel))?;
            }
            Ok(())
        })
    }

    /// The next frame on `channel` is a keyframe, for example after a geometry change.
    pub fn require_keyframe(&self, channel: Channel) {
        if let Some(slot) = self.lock().channels.get_mut(&channel) {
            slot.keyframe = true;
        }
    }

    /// Returns the channel's credit. Acknowledging a frame that is not in flight is a violation;
    /// an acknowledgement for a channel the server already forgot is ignored.
    pub fn acknowledge(&self, channel: Channel, frame: u64) -> Result<(), OutboxError> {
        self.with(|state| {
            let Some(slot) = state.channels.get_mut(&channel) else {
                return Ok(());
            };
            if slot.in_flight.map(|(sent, _)| sent) != Some(frame) {
                return Err(OutboxError::Violation);
            }
            slot.in_flight = None;
            slot.acknowledged = Some(frame);
            if slot.pending.is_some() {
                if !slot.queued {
                    slot.queued = true;
                    state.push_l1(Entry::Frame(channel))?;
                }
            } else if let Some(frames) = slot.end.take() {
                state.channels.remove(&channel);
                state.closed.push(channel);
                state.push_l1(Entry::Message(frames))?;
            }
            Ok(())
        })
    }

    /// Records that the I/O thread wrote a frame. Starts the acknowledgement timer of every frame
    /// taken but not yet written.
    pub fn written(&self, now: Instant) {
        for slot in self.lock().channels.values_mut() {
            if let Some((_, since @ None)) = &mut slot.in_flight {
                *since = Some(now);
            }
        }
    }

    /// When the oldest unacknowledged frame was written, for [`super::health::Liveness::check`].
    pub fn unacknowledged_since(&self) -> Option<Instant> {
        self.lock()
            .channels
            .values()
            .filter_map(|slot| slot.in_flight.and_then(|(_, since)| since))
            .min()
    }

    /// The next whole frame to write and its level. Only the connection's I/O thread calls this.
    /// It encodes presentation outside the outbox lock. A frame this server cannot encode ends
    /// its channel with `PaneEvent::Failed`; the connection continues.
    pub fn next_frame(&self) -> Option<(Vec<u8>, Priority)> {
        loop {
            let taken = self.lock().take();
            let (ticket, presentation, end) = match taken? {
                Taken::Frame(frame, priority) => return Some((frame, priority)),
                Taken::Skip => continue,
                Taken::Present(ticket, presentation, end) => (ticket, presentation, end),
            };
            let encoded = match presentation.encode(ticket) {
                Some(presented) => Some(encode(&ServerMessage::Frame(Box::new(Frame {
                    channel: ticket.channel,
                    frame: ticket.frame,
                    base: ticket.base,
                    input: presented.input,
                    update: presented.update,
                })))),
                None => None,
            };
            let mut state = self.lock();
            let frames = match encoded {
                Some(Ok(frames)) => frames,
                None if end.is_none() => {
                    state.unsent(ticket);
                    continue;
                }
                // The channel is ending: its own last event follows instead of `Failed`.
                None | Some(Err(_)) if end.is_some() => Vec::new(),
                None => Vec::new(),
                Some(Err(_)) => {
                    state.channels.remove(&ticket.channel);
                    state.closed.push(ticket.channel);
                    let failed = encode(&ServerMessage::Pane(PaneMessage {
                        channel: ticket.channel,
                        event: PaneEvent::Failed,
                    }));
                    failed.unwrap_or_default()
                }
            };
            let mut frames = frames.into_iter().chain(end.into_iter().flatten());
            let Some(first) = frames.next() else {
                continue;
            };
            for (index, frame) in frames.enumerate() {
                state.l1_rest.insert(index, frame);
            }
            return Some((first, Priority::L1));
        }
    }
}

fn encode(message: &ServerMessage) -> Result<Frames, OutboxError> {
    encode_server(message)
        .map(|wire| wire.frames)
        .map_err(OutboxError::Encode)
}

impl State {
    fn open(&self) -> Result<(), OutboxError> {
        if self.closing {
            Err(OutboxError::Closed)
        } else {
            Ok(())
        }
    }

    /// A queue passed its bound: the outbox queues `Goodbye(Stalled)` past every bound and
    /// closes itself, so no producer's error is lost silently.
    fn overflow(&mut self) -> OutboxError {
        if !self.closing {
            if let Ok(frames) = encode(&ServerMessage::Goodbye(Fatal::Stalled)) {
                self.l0.extend(frames);
            }
            self.closing = true;
        }
        OutboxError::Stalled
    }

    fn push_l0(&mut self, message: &ServerMessage) -> Result<(), OutboxError> {
        self.open()?;
        let frames = encode(message)?;
        let bytes: usize = frames.iter().map(Vec::len).sum();
        if self.l0.len() + frames.len() > limits::MAX_L0_FRAMES
            || self.l0_bytes + bytes > limits::MAX_L0_BYTES
        {
            return Err(self.overflow());
        }
        self.l0_bytes += bytes;
        self.l0.extend(frames);
        Ok(())
    }

    fn push_l1(&mut self, entry: Entry) -> Result<(), OutboxError> {
        self.open()?;
        if self.l1.len() >= limits::MAX_L1_MESSAGES {
            return Err(self.overflow());
        }
        self.l1.push_back(entry);
        Ok(())
    }

    fn take(&mut self) -> Option<Taken> {
        if let Some(frame) = self.l0.pop_front() {
            self.l0_bytes = self.l0_bytes.saturating_sub(frame.len());
            return Some(Taken::Frame(frame, Priority::L0));
        }
        if self.closing {
            return None;
        }
        if let Some(frame) = self.l1_rest.pop_front() {
            return Some(Taken::Frame(frame, Priority::L1));
        }
        if let Some(entry) = self.l1.pop_front() {
            let frames = match entry {
                Entry::Message(frames) => frames,
                Entry::Inventory => {
                    self.inventory_queued = false;
                    self.inventory.take().unwrap_or_default()
                }
                Entry::Frame(channel) => return Some(self.present(channel)),
            };
            return Some(self.first(frames, Priority::L1));
        }
        if let Some(frame) = self.l2_rest.pop_front() {
            return Some(Taken::Frame(frame, Priority::L2));
        }
        let frames = self.l2.pop_front()?;
        Some(self.first(frames, Priority::L2))
    }

    /// Returns the first frame of a message and keeps the rest ahead of its level's queue.
    fn first(&mut self, frames: Frames, priority: Priority) -> Taken {
        let mut frames = frames.into_iter();
        let Some(first) = frames.next() else {
            return Taken::Skip;
        };
        match priority {
            Priority::L2 => self.l2_rest.extend(frames),
            _ => self.l1_rest.extend(frames),
        }
        Taken::Frame(first, priority)
    }

    fn present(&mut self, channel: Channel) -> Taken {
        let Some(slot) = self.channels.get_mut(&channel) else {
            return Taken::Skip;
        };
        slot.queued = false;
        if !slot.visible || slot.in_flight.is_some() {
            return Taken::Skip;
        }
        let Some(presentation) = slot.pending.take() else {
            return Taken::Skip;
        };
        let ticket = FrameTicket {
            channel,
            frame: slot.next,
            base: if slot.keyframe {
                None
            } else {
                slot.acknowledged
            },
        };
        slot.next += 1;
        slot.keyframe = false;
        slot.in_flight = Some((ticket.frame, None));
        let end = slot.end.take();
        if end.is_some() {
            self.channels.remove(&channel);
            self.closed.push(channel);
        }
        Taken::Present(ticket, presentation, end)
    }

    /// Undoes `present` for a state that equalled its base.
    fn unsent(&mut self, ticket: FrameTicket) {
        if let Some(slot) = self.channels.get_mut(&ticket.channel)
            && slot.in_flight.map(|(sent, _)| sent) == Some(ticket.frame)
        {
            slot.in_flight = None;
            slot.keyframe |= ticket.base.is_none();
            if slot.pending.is_some() && !slot.queued {
                slot.queued = true;
                // A full L1 closes the outbox with `Goodbye(Stalled)`, so the slot is never
                // left queued without an entry on a live connection.
                let _ = self.push_l1(Entry::Frame(ticket.channel));
            }
        }
    }
}

#[cfg(test)]
mod tests;
