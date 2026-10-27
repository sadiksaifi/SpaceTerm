//! One full-duplex SpaceTerm Protocol connection between a client and a server.
//!
//! - [`carrier`] moves whole frames over one socket: account-local IPC, paired WSS, and later
//!   QUIC. [`loopback`] is an in-memory carrier for tests.
//! - [`outbox`] orders a server's outgoing messages by priority and holds the latest presentation
//!   per channel under one frame of credit.
//! - [`server`] applies the protocol's ordering rules to one admitted connection and routes the
//!   rest to the [`service::PaneService`].
//! - `link` is the client's [`link::ServerLink`]: concurrent requests, fire-and-forget input,
//!   per-channel delivery, pushed inventory and health.
//! - [`health`] judges liveness from traffic on the live connection.
//!
//! Each connection has one I/O thread that waits on carrier readiness and a wake descriptor. No
//! code here sleeps or polls on a timer.
pub mod carrier;
pub mod health;
#[cfg(feature = "client")]
pub mod link;
pub mod loopback;
pub mod outbox;
pub mod server;
pub mod service;
#[cfg(all(test, feature = "client"))]
mod tests;

/// Wakes a connection's I/O thread after another thread queued work for it. Implementations
/// must not block, for example by writing one byte to a nonblocking pipe.
pub trait Wake: Send + Sync {
    fn wake(&self);
}

impl<F: Fn() + Send + Sync> Wake for F {
    fn wake(&self) {
        self()
    }
}
