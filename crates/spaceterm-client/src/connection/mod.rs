//! The work link and its QUIC carrier.

pub mod carrier;
pub mod health;
pub mod io;
pub mod link;
pub mod loopback;
pub mod path;
pub mod quic;

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
