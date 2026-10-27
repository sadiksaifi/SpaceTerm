//! The seam between protocol logic and one socket.
//!
//! A carrier moves whole frames ([`crate::protocol::frame`]) in both directions without blocking.
//! The connection's I/O thread waits on [`Carrier::readiness`] and calls the carrier only when a
//! descriptor is ready or the outbox has work. Carriers never sleep or poll on a timer.
//!
//! Implementations: `UnixCarrier` for account-local IPC and `WssCarrier` for paired WSS (rustls and
//! tungstenite on the nonblocking TCP descriptor, `TCP_NODELAY`, `TCP_NOTSENT_LOWAT` of 16 KiB, one
//! WebSocket binary message per frame). A later QUIC carrier exposes a wake descriptor that its
//! runtime signals.
use crate::protocol::{DecodeError, RawFrame};
use std::os::fd::RawFd;

pub trait Carrier: Send {
    /// Offers one whole frame. `Accepted` means the carrier owns the frame and finishes writing it
    /// during later `flush` calls. `Full` means it took nothing; offer the frame again after the
    /// descriptors in [`Carrier::readiness`] report progress.
    fn offer(&mut self, frame: &[u8]) -> Result<Offer, CarrierError>;
    /// Writes buffered bytes without blocking.
    fn flush(&mut self) -> Result<Flush, CarrierError>;
    /// The next whole frame, if one arrived. `Ok(None)` means wait for readiness.
    fn receive(&mut self) -> Result<Option<RawFrame>, CarrierError>;
    /// The descriptors the I/O thread waits on before calling the carrier again.
    fn readiness(&self) -> Readiness;
    /// Starts an orderly close. Later calls fail with `Closed`.
    fn close(&mut self);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Offer {
    Accepted,
    Full,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flush {
    Done,
    Pending,
}

/// `read` becomes readable when `receive` may return a frame, or when a `Full` carrier has room.
/// `write`, when present, becomes writable when buffered bytes can make progress.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Readiness {
    pub read: RawFd,
    pub write: Option<RawFd>,
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
