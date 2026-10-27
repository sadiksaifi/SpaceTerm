//! The server's half of one admitted connection: protocol rules that need no `PaneService`.
//!
//! The I/O thread passes each received frame to [`AdmittedConnection::receive`]. It handles
//! continuation frames, bounds, request ids, channel and serial order, frame acknowledgements,
//! pings, and the account-administration refusal for paired peers. What remains is an
//! [`Inbound`] message for the service. The I/O thread locks the service, calls
//! [`AdmittedConnection::dispatch`], unlocks it, and then runs any returned [`Deferred`] work.
use super::health::{Check, Death, Liveness};
use super::outbox::{Outbox, OutboxError};
use super::service::{AfterLock, Dispatch, PaneService, Peer};
use crate::protocol::frame::Assembler;
use crate::protocol::{
    Channel, ClientMessage, EffectDone, Fatal, Input, Operation, RawFrame, Refusal, Request,
    RequestId, View, limits,
};
use std::collections::BTreeMap;
use std::time::Instant;

/// A client message the service handles.
#[derive(Debug)]
pub enum Inbound {
    /// `Operation::Direct` reaches the host's direct-network state instead of the service.
    Request(Box<Request>),
    Input(Input),
    View(View),
    EffectDone(EffectDone),
    Acknowledge {
        sequence: u64,
    },
    Cancel {
        id: RequestId,
    },
}

/// Reply work to run after the `PaneService` lock is released.
pub struct Deferred {
    id: RequestId,
    work: AfterLock,
}

impl Deferred {
    pub fn run(self, connection: &AdmittedConnection) -> Result<(), Fatal> {
        Ok(connection.outbox.reply(self.id, (self.work)())?)
    }
}

pub struct AdmittedConnection {
    peer: Peer,
    outbox: Outbox,
    assembler: Assembler,
    highest_channel: u32,
    /// The last Input serial per recent channel. The oldest entry is evicted past the bound, and
    /// the service discards input on channels it no longer binds.
    serials: BTreeMap<Channel, u32>,
    liveness: Liveness,
}

impl AdmittedConnection {
    pub fn new(peer: Peer, outbox: Outbox, now: Instant) -> Self {
        Self {
            peer,
            outbox,
            assembler: Assembler::default(),
            highest_channel: 0,
            serials: BTreeMap::new(),
            liveness: Liveness::new(now),
        }
    }

    pub fn peer(&self) -> &Peer {
        &self.peer
    }

    pub fn outbox(&self) -> &Outbox {
        &self.outbox
    }

    /// Applies one received frame. An error ends the connection with `Goodbye(error)`.
    pub fn receive(&mut self, frame: RawFrame, now: Instant) -> Result<Option<Inbound>, Fatal> {
        self.liveness.received(now);
        let Some(message) = self.assembler.client(frame)? else {
            return Ok(None);
        };
        Ok(match message {
            ClientMessage::Request(request) => {
                self.outbox.begin(request.id)?;
                if let Operation::Claim(claim) = &request.operation {
                    if claim.channel.0 <= self.highest_channel {
                        return Err(Fatal::ProtocolViolation);
                    }
                    self.highest_channel = claim.channel.0;
                    self.serials.insert(claim.channel, 0);
                    if self.serials.len() > limits::MAX_CHANNELS {
                        self.serials.pop_first();
                    }
                }
                if self.peer.paired && administration(&request.operation) {
                    self.outbox.reply(request.id, Err(Refusal::Unsupported))?;
                    return Ok(None);
                }
                Some(Inbound::Request(request))
            }
            ClientMessage::Input(input) => {
                self.claimed(input.channel)?;
                if let Some(last) = self.serials.get_mut(&input.channel) {
                    if Some(input.serial) != last.checked_add(1) {
                        return Err(Fatal::ProtocolViolation);
                    }
                    *last = input.serial;
                }
                Some(Inbound::Input(input))
            }
            ClientMessage::FrameAck(ack) => {
                self.claimed(ack.channel)?;
                self.outbox.acknowledge(ack.channel, ack.frame)?;
                None
            }
            ClientMessage::View(view) => {
                self.claimed(view.channel)?;
                self.outbox.view(view.channel, view.visible);
                Some(Inbound::View(view))
            }
            ClientMessage::EffectDone(done) => {
                self.claimed(done.channel)?;
                Some(Inbound::EffectDone(done))
            }
            ClientMessage::Acknowledge(acknowledge) => Some(Inbound::Acknowledge {
                sequence: acknowledge.sequence,
            }),
            ClientMessage::Cancel(cancel) => Some(Inbound::Cancel { id: cancel.id }),
            ClientMessage::Ping(ping) => {
                self.outbox.pong(ping.nonce)?;
                None
            }
            ClientMessage::Pong(_) => None,
        })
    }

    /// Channel messages may name only channels a `Claim` already introduced.
    fn claimed(&self, channel: Channel) -> Result<(), Fatal> {
        if channel.0 == 0 || channel.0 > self.highest_channel {
            return Err(Fatal::ProtocolViolation);
        }
        Ok(())
    }

    /// Calls the service while the caller holds its lock.
    pub fn dispatch(
        &self,
        inbound: Inbound,
        service: &mut dyn PaneService,
    ) -> Result<Option<Deferred>, Fatal> {
        let peer = &self.peer;
        match inbound {
            Inbound::Request(request) => {
                let Request { id, operation } = *request;
                match service.request(peer, id, operation) {
                    Dispatch::Reply(result) => self.outbox.reply(id, *result)?,
                    Dispatch::Deferred => {}
                    Dispatch::After(work) => return Ok(Some(Deferred { id, work })),
                }
            }
            Inbound::Input(input) => service.input(peer, input),
            Inbound::View(view) => service.view(peer, view),
            Inbound::EffectDone(done) => service.effect_done(peer, done),
            Inbound::Acknowledge { sequence } => service.acknowledge(peer, sequence),
            Inbound::Cancel { id } => service.cancel(peer, id),
        }
        Ok(None)
    }

    /// Records whether outbox frames wait on the carrier after a write attempt.
    pub fn wrote(&mut self, now: Instant, blocked: bool) {
        self.liveness.wrote(now, blocked);
    }

    /// Pings or ends the connection from traffic, and returns when to check again.
    pub fn check(&mut self, now: Instant) -> Result<Instant, Death> {
        match self.liveness.check(now, self.outbox.unacknowledged_since()) {
            Check::Live { next } => Ok(next),
            Check::Ping { nonce, next } => match self.outbox.ping(nonce) {
                Ok(()) | Err(OutboxError::Closed) => Ok(next),
                Err(_) => Err(Death::Stalled),
            },
            Check::Dead(death) => Err(death),
        }
    }
}

fn administration(operation: &Operation) -> bool {
    matches!(
        operation,
        Operation::Shutdown
            | Operation::Mutate {
                mutation: crate::hierarchy::Mutation::EndAllSessions { .. },
                ..
            }
    )
}
