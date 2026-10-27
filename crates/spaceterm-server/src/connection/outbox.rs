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
//! Each channel has one frame of credit, returned by its acknowledgement. Presentation is
//! encoded by [`Presentation::encode`] on the calling I/O thread, outside the `PaneService` lock
//! and outside this outbox's lock.
use super::Wake;
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
    /// A queue passed its bound. The connection ends with `Goodbye(Stalled)`.
    #[error("the connection stalled")]
    Stalled,
    /// The connection has too many channels or requests open.
    #[error("the connection reached capacity")]
    Capacity,
    /// The peer broke an ordering rule, such as acknowledging a frame it was not sent.
    #[error("the peer broke the protocol")]
    Violation,
    /// The connection is closing. Nothing more is queued except what already was.
    #[error("the connection is closing")]
    Closed,
    #[error("the message could not be encoded")]
    Encode(EncodeError),
}

impl From<OutboxError> for Fatal {
    fn from(error: OutboxError) -> Self {
        match error {
            OutboxError::Stalled => Self::Stalled,
            OutboxError::Capacity => Self::Capacity,
            OutboxError::Violation | OutboxError::Encode(_) => Self::ProtocolViolation,
            OutboxError::Closed => Self::Stopping,
        }
    }
}

/// Clones share one connection's queues.
#[derive(Clone)]
pub struct Outbox {
    shared: Arc<Shared>,
}

struct Shared {
    state: Mutex<State>,
    wake: Arc<dyn Wake>,
}

#[derive(Default)]
struct State {
    l0: VecDeque<Vec<u8>>,
    l0_bytes: usize,
    l1: VecDeque<Entry>,
    l1_rest: VecDeque<Vec<u8>>,
    l2: VecDeque<ServerMessage>,
    l2_rest: VecDeque<Vec<u8>>,
    inventory: Option<InventoryRevision>,
    inventory_queued: bool,
    channels: BTreeMap<Channel, Slot>,
    requests: BTreeSet<RequestId>,
    closing: bool,
}

enum Entry {
    Message(ServerMessage),
    Inventory,
    Frame(Channel),
}

struct Slot {
    pending: Option<Box<dyn Presentation>>,
    queued: bool,
    visible: bool,
    next: u64,
    in_flight: Option<(u64, Instant)>,
    acknowledged: Option<u64>,
    keyframe: bool,
}

enum Taken {
    Frame(Vec<u8>),
    Message(ServerMessage, bool),
    Present(FrameTicket, Box<dyn Presentation>),
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

    fn with<T>(
        &self,
        change: impl FnOnce(&mut State) -> Result<T, OutboxError>,
    ) -> Result<T, OutboxError> {
        let result = change(&mut self.shared.state.lock().unwrap_or_else(|e| e.into_inner()));
        if result.is_ok() {
            self.shared.wake.wake();
        }
        result
    }

    /// Registers a request id. A duplicate in-flight id is a violation.
    pub fn begin(&self, id: RequestId) -> Result<(), OutboxError> {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.requests.len() >= limits::MAX_REQUESTS_IN_FLIGHT {
            return Err(OutboxError::Violation);
        }
        if !state.requests.insert(id) {
            return Err(OutboxError::Violation);
        }
        Ok(())
    }

    /// Queues the one reply to `id`. Bulk bodies go to L2.
    pub fn reply(
        &self,
        id: RequestId,
        result: Result<ReplyBody, Refusal>,
    ) -> Result<(), OutboxError> {
        self.with(|state| {
            if !state.requests.remove(&id) {
                return Err(OutboxError::Violation);
            }
            let bulk = matches!(&result, Ok(body) if body.bulk());
            let message = ServerMessage::Reply(Box::new(Reply { id, result }));
            if bulk {
                state.open()?;
                if state.l2.len() >= limits::MAX_L2_MESSAGES {
                    return Err(OutboxError::Stalled);
                }
                state.l2.push_back(message);
                Ok(())
            } else {
                state.push_l1(Entry::Message(message))
            }
        })
    }

    pub fn pane(&self, channel: Channel, event: PaneEvent) -> Result<(), OutboxError> {
        self.with(|state| {
            state.push_l1(Entry::Message(ServerMessage::Pane(PaneMessage {
                channel,
                event,
            })))
        })
    }

    /// Replaces any inventory revision not yet written. A reply queued after this call is
    /// written after the revision, so a deferred outcome never precedes the revision holding it.
    pub fn inventory(&self, revision: InventoryRevision) -> Result<(), OutboxError> {
        self.with(|state| {
            state.open()?;
            state.inventory = Some(revision);
            if !state.inventory_queued {
                state.push_l1(Entry::Inventory)?;
                state.inventory_queued = true;
            }
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
    /// written.
    pub fn goodbye(&self, fatal: Fatal) -> Result<(), OutboxError> {
        self.with(|state| {
            state.open()?;
            let message =
                encode_server(&ServerMessage::Goodbye(fatal)).map_err(OutboxError::Encode)?;
            state.l0.extend(message.frames);
            state.closing = true;
            Ok(())
        })
    }

    /// Starts presenting `channel`, visible and owing a keyframe.
    pub fn open(&self, channel: Channel) -> Result<(), OutboxError> {
        self.with(|state| {
            if state.channels.len() >= limits::MAX_CHANNELS {
                return Err(OutboxError::Capacity);
            }
            if state.channels.contains_key(&channel) {
                return Err(OutboxError::Violation);
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
                },
            );
            Ok(())
        })
    }

    /// Stops presenting `channel` and drops its pending state.
    pub fn close(&self, channel: Channel) {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state.channels.remove(&channel);
    }

    /// A hidden channel drops its pending state and receives no frames. Revealing keeps the
    /// acknowledged baseline; the publisher presents the current state again.
    pub fn view(&self, channel: Channel, visible: bool) {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(slot) = state.channels.get_mut(&channel) {
            slot.visible = visible;
            if !visible {
                slot.pending = None;
            }
        }
    }

    /// Replaces the channel's pending state. Ignored for a hidden or closed channel.
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
            if !slot.visible {
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
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(slot) = state.channels.get_mut(&channel) {
            slot.keyframe = true;
        }
    }

    /// Returns the channel's credit. Acknowledging a frame that is not in flight is a violation;
    /// an acknowledgement for a closed channel is ignored.
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
            if slot.pending.is_some() && !slot.queued {
                slot.queued = true;
                state.push_l1(Entry::Frame(channel))?;
            }
            Ok(())
        })
    }

    /// When the oldest unacknowledged frame was written, for [`super::health::Liveness::check`].
    pub fn unacknowledged_since(&self) -> Option<Instant> {
        let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state
            .channels
            .values()
            .filter_map(|slot| slot.in_flight.map(|(_, since)| since))
            .min()
    }

    /// The next whole frame to write, in priority order. Only the connection's I/O thread calls
    /// this. It encodes outside the outbox lock.
    pub fn next_frame(&self, now: Instant) -> Result<Option<Vec<u8>>, OutboxError> {
        loop {
            let taken = {
                let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
                state.take(now)
            };
            let (message, bulk) = match taken {
                None => return Ok(None),
                Some(Taken::Frame(frame)) => return Ok(Some(frame)),
                Some(Taken::Skip) => continue,
                Some(Taken::Message(message, bulk)) => (message, bulk),
                Some(Taken::Present(ticket, presentation)) => match presentation.encode(ticket) {
                    Some(presented) => (
                        ServerMessage::Frame(Box::new(Frame {
                            channel: ticket.channel,
                            frame: ticket.frame,
                            base: ticket.base,
                            input: presented.input,
                            update: presented.update,
                        })),
                        false,
                    ),
                    None => {
                        self.unsent(ticket);
                        continue;
                    }
                },
            };
            let mut frames = encode_server(&message)
                .map_err(OutboxError::Encode)?
                .frames
                .into_iter();
            let first = frames.next();
            let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            let rest = if bulk {
                &mut state.l2_rest
            } else {
                &mut state.l1_rest
            };
            for (index, frame) in frames.enumerate() {
                rest.insert(index, frame);
            }
            return Ok(first);
        }
    }

    fn unsent(&self, ticket: FrameTicket) {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(slot) = state.channels.get_mut(&ticket.channel)
            && slot.in_flight.map(|(sent, _)| sent) == Some(ticket.frame)
        {
            slot.in_flight = None;
            slot.keyframe = ticket.base.is_none();
            if slot.pending.is_some() && !slot.queued {
                slot.queued = true;
                let _ = state.push_l1(Entry::Frame(ticket.channel));
            }
        }
    }
}

impl State {
    fn open(&self) -> Result<(), OutboxError> {
        if self.closing {
            Err(OutboxError::Closed)
        } else {
            Ok(())
        }
    }

    fn push_l0(&mut self, message: &ServerMessage) -> Result<(), OutboxError> {
        self.open()?;
        let message = encode_server(message).map_err(OutboxError::Encode)?;
        if self.l0.len() + message.frames.len() > limits::MAX_L0_FRAMES
            || self.l0_bytes + message.bytes() > limits::MAX_L0_BYTES
        {
            return Err(OutboxError::Stalled);
        }
        self.l0_bytes += message.bytes();
        self.l0.extend(message.frames);
        Ok(())
    }

    fn push_l1(&mut self, entry: Entry) -> Result<(), OutboxError> {
        self.open()?;
        if self.l1.len() >= limits::MAX_L1_MESSAGES {
            return Err(OutboxError::Stalled);
        }
        self.l1.push_back(entry);
        Ok(())
    }

    fn take(&mut self, now: Instant) -> Option<Taken> {
        if let Some(frame) = self.l0.pop_front() {
            self.l0_bytes = self.l0_bytes.saturating_sub(frame.len());
            return Some(Taken::Frame(frame));
        }
        if self.closing {
            return None;
        }
        if let Some(frame) = self.l1_rest.pop_front() {
            return Some(Taken::Frame(frame));
        }
        if let Some(entry) = self.l1.pop_front() {
            return Some(match entry {
                Entry::Message(message) => Taken::Message(message, false),
                Entry::Inventory => {
                    self.inventory_queued = false;
                    match self.inventory.take() {
                        Some(revision) => {
                            Taken::Message(ServerMessage::Inventory(Box::new(revision)), false)
                        }
                        None => Taken::Skip,
                    }
                }
                Entry::Frame(channel) => self.present(channel, now),
            });
        }
        if let Some(frame) = self.l2_rest.pop_front() {
            return Some(Taken::Frame(frame));
        }
        self.l2
            .pop_front()
            .map(|message| Taken::Message(message, true))
    }

    fn present(&mut self, channel: Channel, now: Instant) -> Taken {
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
        slot.in_flight = Some((ticket.frame, now));
        Taken::Present(ticket, presentation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::frame::Assembler;
    use crate::protocol::{RawFrame, ScreenUpdate};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct State(u64);
    impl Presentation for State {
        fn encode(self: Box<Self>, _ticket: FrameTicket) -> Option<Presented> {
            Some(Presented {
                input: self.0 as u32,
                update: ScreenUpdate {
                    generation: self.0,
                    ..ScreenUpdate::default()
                },
            })
        }
    }
    struct Unchanged;
    impl Presentation for Unchanged {
        fn encode(self: Box<Self>, _ticket: FrameTicket) -> Option<Presented> {
            None
        }
    }

    fn fresh() -> (Outbox, Arc<AtomicUsize>) {
        let wakes = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&wakes);
        (
            Outbox::new(Arc::new(move || {
                counter.fetch_add(1, Ordering::Relaxed);
            })),
            wakes,
        )
    }

    fn drain(outbox: &Outbox, now: Instant) -> Vec<ServerMessage> {
        let mut assembler = Assembler::default();
        let mut messages = Vec::new();
        while let Some(frame) = outbox.next_frame(now).unwrap() {
            if let Some(message) = assembler.server(RawFrame::parse(&frame).unwrap()).unwrap() {
                messages.push(message);
            }
        }
        messages
    }

    #[test]
    fn one_frame_of_credit_and_newer_state_replaces_pending_state() {
        let (outbox, wakes) = fresh();
        let now = Instant::now();
        let channel = Channel(1);
        outbox.open(channel).unwrap();
        outbox.present(channel, Box::new(State(1))).unwrap();
        outbox.present(channel, Box::new(State(2))).unwrap();
        assert!(wakes.load(Ordering::Relaxed) >= 2);
        let sent = drain(&outbox, now);
        let [ServerMessage::Frame(first)] = sent.as_slice() else {
            panic!("expected one frame");
        };
        assert_eq!((first.frame, first.base, first.input), (1, None, 2));

        outbox.present(channel, Box::new(State(3))).unwrap();
        outbox.present(channel, Box::new(State(4))).unwrap();
        assert!(
            drain(&outbox, now).is_empty(),
            "no credit before the acknowledgement"
        );
        assert_eq!(outbox.unacknowledged_since(), Some(now));
        assert_eq!(outbox.acknowledge(channel, 2), Err(OutboxError::Violation));
        outbox.acknowledge(channel, 1).unwrap();
        let sent = drain(&outbox, now);
        let [ServerMessage::Frame(second)] = sent.as_slice() else {
            panic!("expected one frame");
        };
        assert_eq!((second.frame, second.base, second.input), (2, Some(1), 4));

        outbox.acknowledge(channel, 2).unwrap();
        outbox.require_keyframe(channel);
        outbox.present(channel, Box::new(Unchanged)).unwrap();
        assert!(drain(&outbox, now).is_empty());
        assert_eq!(
            outbox.unacknowledged_since(),
            None,
            "an unchanged state keeps the credit"
        );
        outbox.present(channel, Box::new(State(5))).unwrap();
        let sent = drain(&outbox, now);
        let [ServerMessage::Frame(third)] = sent.as_slice() else {
            panic!("expected one frame");
        };
        assert_eq!(
            third.base, None,
            "the required keyframe survives an unchanged state"
        );
    }

    #[test]
    fn hidden_channels_receive_no_frames() {
        let (outbox, _) = fresh();
        let channel = Channel(4);
        outbox.open(channel).unwrap();
        outbox.present(channel, Box::new(State(1))).unwrap();
        outbox.view(channel, false);
        outbox.present(channel, Box::new(State(2))).unwrap();
        assert!(drain(&outbox, Instant::now()).is_empty());
        outbox.view(channel, true);
        outbox.present(channel, Box::new(State(3))).unwrap();
        assert_eq!(drain(&outbox, Instant::now()).len(), 1);
    }

    #[test]
    fn publication_order_holds_within_l1_and_l0_preempts_l2_chunks() {
        let (outbox, _) = fresh();
        let now = Instant::now();
        let channel = Channel(1);
        outbox.open(channel).unwrap();
        for id in 1..=3 {
            outbox.begin(RequestId(id)).unwrap();
        }
        let page = crate::protocol::Page {
            epoch: 1,
            start: 0,
            rows: vec![crate::protocol::screen::Row {
                index: 0,
                wrapped: false,
                runs: vec![crate::protocol::screen::Run {
                    style: 0,
                    link: None,
                    text: "x".repeat(150 * 1024),
                    cells: crate::protocol::screen::Cells::Narrow,
                }],
            }],
            styles: vec![],
            links: vec![],
        };
        outbox
            .reply(RequestId(1), Ok(ReplyBody::Page(page)))
            .unwrap();
        outbox
            .inventory(InventoryRevision {
                inventory: crate::hierarchy::State::empty().inventory,
                next_sequence: 1,
            })
            .unwrap();
        outbox.reply(RequestId(2), Ok(ReplyBody::Done)).unwrap();
        outbox.present(channel, Box::new(State(1))).unwrap();
        outbox.pane(channel, PaneEvent::Ended).unwrap();
        assert_eq!(
            outbox.reply(RequestId(2), Ok(ReplyBody::Done)),
            Err(OutboxError::Violation),
            "exactly one reply per request"
        );

        let mut assembler = Assembler::default();
        let mut order = Vec::new();
        let mut frames = 0;
        while let Some(frame) = outbox.next_frame(now).unwrap() {
            frames += 1;
            if frames == 6 {
                outbox.ping(9).unwrap();
            }
            let frame = RawFrame::parse(&frame).unwrap();
            assert!(frame.body.len() <= limits::MAX_FRAME_BODY);
            if let Some(message) = assembler.server(frame).unwrap() {
                order.push(match message {
                    ServerMessage::Inventory(_) => "inventory",
                    ServerMessage::Reply(reply) if reply.id == RequestId(2) => "reply",
                    ServerMessage::Frame(_) => "frame",
                    ServerMessage::Pane(_) => "pane",
                    ServerMessage::Ping(_) => "ping",
                    ServerMessage::Reply(reply) if reply.id == RequestId(1) => "page",
                    _ => "other",
                });
            }
        }
        assert_eq!(
            order,
            ["inventory", "reply", "frame", "pane", "ping", "page"]
        );
    }

    #[test]
    fn goodbye_is_last_and_bounds_stall_the_connection() {
        let (outbox, _) = fresh();
        outbox.ping(1).unwrap();
        outbox.goodbye(Fatal::Stopping).unwrap();
        assert_eq!(
            outbox.pane(Channel(1), PaneEvent::Ended),
            Err(OutboxError::Closed)
        );
        let sent = drain(&outbox, Instant::now());
        assert!(matches!(
            sent.as_slice(),
            [
                ServerMessage::Ping(_),
                ServerMessage::Goodbye(Fatal::Stopping)
            ]
        ));

        let (outbox, _) = fresh();
        let result = (0..=limits::MAX_L0_FRAMES as u64).try_for_each(|nonce| outbox.pong(nonce));
        assert_eq!(result, Err(OutboxError::Stalled));
        for channel in 0..limits::MAX_CHANNELS as u32 {
            outbox.open(Channel(channel)).unwrap();
        }
        assert_eq!(outbox.open(Channel(999)), Err(OutboxError::Capacity));
    }
}
