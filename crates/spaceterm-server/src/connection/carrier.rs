//! The seam between protocol logic and one socket.
//!
//! A carrier moves whole frames ([`crate::protocol::frame`]) in both directions without blocking.
//! The connection's I/O thread waits on [`Carrier::readiness`] and calls the carrier only when a
//! descriptor is ready or the outbox has work. Carriers never sleep or poll on a timer.
//!
//! The I/O loop follows these rules:
//!
//! - After any readiness event it calls [`Carrier::receive`] until `Ok(None)`. The QUIC carrier
//!   holds decrypted frames that no descriptor reports as readable.
//! - It offers the next frame only after the previous offer was `Accepted`, and it calls
//!   [`Carrier::flush`] after every accepted offer and whenever the write descriptor is ready.
//! - After [`Carrier::close`] it keeps calling `flush` until `Flush::Done` or a stall, so the final
//!   `Goodbye` and the end of every stream leave the socket.
//!
//! Implementations: `UnixCarrier` for account-local IPC and `QuicCarrier` for paired connections
//! (noq-proto on a UDP socket the I/O thread drives, one stream per [`Priority`] on the server).
use crate::protocol::{DecodeError, RawFrame};
use std::os::fd::RawFd;

/// The outbox level a frame was taken from. Stream carriers ignore it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Priority {
    /// Pings, pongs, goodbyes; on the client also requests, input, acknowledgements and views.
    L0,
    /// Replies, Pane events, frames and inventory; on the client the remaining messages.
    L1,
    /// Bulk replies.
    L2,
}

pub trait Carrier: Send {
    /// Offers one whole frame. `Accepted` means the carrier owns the frame and finishes writing it
    /// during later `flush` calls. A carrier returns `Full`, taking nothing, while more than one
    /// accepted frame is unflushed, so frames wait in the outbox where priority still applies. A
    /// carrier with a stream per level may refuse one level and accept another.
    /// Offer the frame again after `flush` makes progress or `readiness` reports it.
    fn offer(&mut self, frame: &[u8], priority: Priority) -> Result<Offer, CarrierError>;
    /// Writes buffered bytes without blocking.
    fn flush(&mut self) -> Result<Flush, CarrierError>;
    /// The next whole frame, if one arrived. `Ok(None)` means wait for readiness.
    fn receive(&mut self) -> Result<Option<RawFrame>, CarrierError>;
    /// The descriptors the I/O thread waits on before calling the carrier again.
    fn readiness(&self) -> Readiness;
    /// Starts an orderly close. Later offers and receives fail with `Closed`; `flush` keeps
    /// draining what was accepted, followed by any closing frame of the transport.
    fn close(&mut self);
    /// The peer stopped answering while this end's address may still look usable, as after a
    /// network change. A carrier that can move to a new path does so.
    fn path_suspect(&mut self) {}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Offer {
    Accepted,
    Full,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Flush {
    /// Whether this call wrote any bytes. It feeds `Liveness::wrote`.
    pub progressed: bool,
    /// Whether accepted bytes are still waiting.
    pub pending: bool,
}

/// `read` becomes readable when `receive` may return a frame, or when a `Full` carrier has room.
/// `write`, when present, becomes writable when buffered bytes can make progress. `progress`, when
/// present, becomes readable when accepted frames can make progress, for a carrier whose writes
/// wait on what it receives. `deadline`, when present, is when the carrier must be called again
/// even without readiness, such as a retransmission timer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Readiness {
    pub read: RawFd,
    pub write: Option<RawFd>,
    pub progress: Option<RawFd>,
    pub deadline: Option<std::time::Instant>,
}

/// Classifications only. Native errors and peer bytes never cross this boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CarrierError {
    #[error("the connection closed")]
    Closed,
    #[error("the connection failed")]
    Failed,
    #[error("the peer sent a malformed frame")]
    Decode(DecodeError),
}

impl From<DecodeError> for CarrierError {
    fn from(error: DecodeError) -> Self {
        Self::Decode(error)
    }
}
