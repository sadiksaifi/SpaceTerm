//! Each channel's latest-state semantic slot: the demand generation, the acknowledged model, the
//! revision in flight and the model waiting for credit.
//!
//! A channel has one semantic credit, separate from its frame credit. A revision is one or more
//! [`Update`] messages. The I/O thread encodes one of them per L1 turn, outside every lock, and
//! queues the rest of the revision at the back of L1, so frames, replies and other Panes
//! interleave with a large revision. A newer model replaces one not yet started; the revision in
//! flight keeps its captured model until the client acknowledges it.
//! [ADR 0062](../../../../../docs/adr/0062-push-terminal-semantics-as-binary-updates.md) owns the
//! exchange.
use super::{Entry, OutboxError, State};
use crate::pane::accessibility::Update;
use crate::protocol::Channel;
use std::any::Any;
use std::sync::Arc;

/// A Pane's semantic model at one revision, captured under the `PaneService` lock. It owns its
/// model, for example through an `Arc`, and never borrows service state.
pub trait Semantics: Any + Send + Sync {
    /// The Pane's semantic revision this model presents. It increases with every change.
    fn revision(&self) -> u64;

    /// Starts encoding this model as the revision `ticket` names. `base` is the model the client
    /// acknowledged in the same demand generation, or `None` for a complete document. `None`
    /// means this model equals `base`: nothing is written and the credit stays.
    fn encode(
        self: Arc<Self>,
        ticket: SemanticTicket,
        base: Option<Arc<dyn Semantics>>,
    ) -> Option<Box<dyn SemanticEncoding>>;
}

/// What the outbox fixes for a revision before it is encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SemanticTicket {
    pub demand_generation: u64,
    pub revision: u64,
    /// The acknowledged revision `base` presents, or `None` for a complete document.
    pub base: Option<u64>,
}

/// The rest of one revision, encoded on the I/O thread one message at a time.
pub trait SemanticEncoding: Send {
    /// The revision's next message. The message with a tail is the last; `next` is not called
    /// again after it.
    fn next(&mut self) -> Update;
}

/// The semantic state of one channel.
#[derive(Default)]
pub(super) struct Slot {
    /// The generation of the channel's last `Demand::Start`, 0 before any. It stays after demand
    /// ends, for as long as the channel is open, so a late acknowledgement is recognized.
    last_start: u64,
    demand: Option<Demand>,
    /// Whether an `Entry::Semantics` of this channel waits in L1.
    queued: bool,
    /// Numbers each started revision, so an encoding that finishes after its revision was
    /// abandoned is dropped instead of written.
    started: u64,
}

/// The state of one standing demand generation.
struct Demand {
    generation: u64,
    /// The revision the client acknowledged last in this generation, and its model.
    acknowledged: Option<(u64, Arc<dyn Semantics>)>,
    /// The newest model not yet started.
    pending: Option<Arc<dyn Semantics>>,
    in_flight: Option<InFlight>,
    /// The highest revision whose tail was taken for writing in this generation.
    sent_high: Option<u64>,
}

/// The revision holding the channel's credit.
struct InFlight {
    serial: u64,
    revision: u64,
    model: Arc<dyn Semantics>,
    rest: Rest,
}

enum Rest {
    /// The revision's remaining messages, waiting for the channel's next L1 turn. A hidden
    /// channel keeps them suspended until it is shown again.
    Ready(Box<dyn SemanticEncoding>),
    /// The I/O thread is encoding the next message.
    Encoding,
    /// The tail was taken for writing; the revision waits for its acknowledgement.
    Written,
}

/// One L1 turn of semantic work, done outside the outbox lock.
pub(super) struct Work {
    pub(super) channel: Channel,
    serial: u64,
    step: Step,
}

enum Step {
    Begin {
        model: Arc<dyn Semantics>,
        ticket: SemanticTicket,
        base: Option<Arc<dyn Semantics>>,
    },
    Continue(Box<dyn SemanticEncoding>),
}

/// A message encoded by [`Work::run`], with what follows it.
pub(super) struct Encoded {
    pub(super) channel: Channel,
    pub(super) serial: u64,
    pub(super) update: Update,
    pub(super) rest: Option<Box<dyn SemanticEncoding>>,
}

impl Work {
    /// Encodes the next message of the revision. `None` means the revision equals its base.
    pub(super) fn run(self) -> Result<Encoded, (Channel, u64)> {
        let Self {
            channel,
            serial,
            step,
        } = self;
        let mut encoding = match step {
            Step::Begin {
                model,
                ticket,
                base,
            } => model.encode(ticket, base).ok_or((channel, serial))?,
            Step::Continue(encoding) => encoding,
        };
        let update = encoding.next();
        let rest = update.tail.is_none().then_some(encoding);
        Ok(Encoded {
            channel,
            serial,
            update,
            rest,
        })
    }
}

impl Slot {
    /// Whether this channel's next L1 turn has semantic work: a suspended revision, or a pending
    /// model with credit available.
    fn has_work(&self) -> bool {
        self.demand
            .as_ref()
            .is_some_and(|demand| match &demand.in_flight {
                Some(flight) => matches!(flight.rest, Rest::Ready(_)),
                None => demand.pending.is_some(),
            })
    }

    /// Forgets the standing demand, its store and any unfinished revision, without waiting for
    /// credit. Keeps `last_start`.
    pub(super) fn end(&mut self) {
        self.demand = None;
    }
}

impl State {
    /// Queues the channel's semantic turn if it has work, is visible and is not already queued.
    pub(super) fn queue_semantics(&mut self, channel: Channel) -> Result<(), OutboxError> {
        let Some(slot) = self.channels.get_mut(&channel) else {
            return Ok(());
        };
        if !slot.visible
            || slot.end.is_some()
            || slot.semantics.queued
            || !slot.semantics.has_work()
        {
            return Ok(());
        }
        slot.semantics.queued = true;
        self.push_l1(Entry::Semantics(channel))
    }

    pub(super) fn start_semantics(
        &mut self,
        channel: Channel,
        generation: u64,
    ) -> Result<(), OutboxError> {
        let Some(slot) = self.channels.get_mut(&channel) else {
            return Ok(());
        };
        if generation <= slot.semantics.last_start {
            return Err(OutboxError::Violation);
        }
        slot.semantics.last_start = generation;
        slot.semantics.demand = Some(Demand {
            generation,
            acknowledged: None,
            pending: None,
            in_flight: None,
            sent_high: None,
        });
        Ok(())
    }

    pub(super) fn end_semantics(&mut self, channel: Channel) {
        if let Some(slot) = self.channels.get_mut(&channel) {
            slot.semantics.end();
        }
    }

    pub(super) fn offer_semantics(
        &mut self,
        channel: Channel,
        model: Arc<dyn Semantics>,
    ) -> Result<(), OutboxError> {
        let Some(demand) = self
            .channels
            .get_mut(&channel)
            .filter(|slot| slot.end.is_none())
            .and_then(|slot| slot.semantics.demand.as_mut())
        else {
            return Ok(());
        };
        demand.pending = Some(model);
        self.queue_semantics(channel)
    }

    /// The acknowledgement rule of ADR 0062, in order.
    pub(super) fn acknowledge_semantics(
        &mut self,
        channel: Channel,
        demand_generation: u64,
        revision: u64,
    ) -> Result<(), OutboxError> {
        // A channel the server already forgot never ends the connection's other channels.
        let Some(slot) = self.channels.get_mut(&channel) else {
            return Ok(());
        };
        if demand_generation > slot.semantics.last_start {
            return Err(OutboxError::Violation);
        }
        let Some(demand) = slot
            .semantics
            .demand
            .as_mut()
            .filter(|demand| demand.generation == demand_generation)
        else {
            // An earlier generation, or demand that ended: no credit.
            return Ok(());
        };
        if let Some(flight) = demand
            .in_flight
            .take_if(|flight| flight.revision == revision && matches!(flight.rest, Rest::Written))
        {
            demand.acknowledged = Some((revision, flight.model));
            return self.queue_semantics(channel);
        }
        // A duplicate, or a revision the client skipped because a later one replaced it.
        if demand.sent_high.is_some_and(|high| revision <= high) {
            return Ok(());
        }
        Err(OutboxError::Violation)
    }

    /// The channel's semantic turn: the next message of its revision in flight, or the start of
    /// its pending model.
    pub(super) fn take_semantics(&mut self, channel: Channel) -> Option<Work> {
        let slot = self.channels.get_mut(&channel)?;
        slot.semantics.queued = false;
        if !slot.visible || slot.end.is_some() {
            return None;
        }
        let semantics = &mut slot.semantics;
        let demand = semantics.demand.as_mut()?;
        if let Some(flight) = &mut demand.in_flight {
            let Rest::Ready(encoding) = std::mem::replace(&mut flight.rest, Rest::Encoding) else {
                return None;
            };
            return Some(Work {
                channel,
                serial: flight.serial,
                step: Step::Continue(encoding),
            });
        }
        let model = demand.pending.take()?;
        let revision = model.revision();
        if demand
            .acknowledged
            .as_ref()
            .is_some_and(|(acknowledged, _)| revision <= *acknowledged)
        {
            return None;
        }
        semantics.started += 1;
        let serial = semantics.started;
        demand.in_flight = Some(InFlight {
            serial,
            revision,
            model: Arc::clone(&model),
            rest: Rest::Encoding,
        });
        Some(Work {
            channel,
            serial,
            step: Step::Begin {
                model,
                ticket: SemanticTicket {
                    demand_generation: demand.generation,
                    revision,
                    base: demand.acknowledged.as_ref().map(|(base, _)| *base),
                },
                base: demand
                    .acknowledged
                    .as_ref()
                    .map(|(_, model)| Arc::clone(model)),
            },
        })
    }

    fn flight(&mut self, channel: Channel, serial: u64) -> Option<&mut Demand> {
        self.channels
            .get_mut(&channel)?
            .semantics
            .demand
            .as_mut()
            .filter(|demand| {
                demand
                    .in_flight
                    .as_ref()
                    .is_some_and(|f| f.serial == serial)
            })
    }

    /// Records that the encoded message is taken for writing. `false` means its revision was
    /// abandoned meanwhile, so the message is dropped.
    pub(super) fn semantics_taken(
        &mut self,
        channel: Channel,
        serial: u64,
        rest: Option<Box<dyn SemanticEncoding>>,
    ) -> Result<bool, OutboxError> {
        let Some(demand) = self.flight(channel, serial) else {
            return Ok(false);
        };
        let flight = demand.in_flight.as_mut().expect("the flight matched");
        match rest {
            Some(rest) => flight.rest = Rest::Ready(rest),
            None => {
                flight.rest = Rest::Written;
                demand.sent_high = Some(flight.revision);
            }
        }
        self.queue_semantics(channel)?;
        Ok(true)
    }

    /// Undoes `take_semantics` for a model that equalled its base.
    pub(super) fn semantics_unsent(&mut self, channel: Channel, serial: u64) {
        if let Some(demand) = self.flight(channel, serial) {
            demand.in_flight = None;
            // A full L1 closes the outbox with `Goodbye(Stalled)`.
            let _ = self.queue_semantics(channel);
        }
    }
}
