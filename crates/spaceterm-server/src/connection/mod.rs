//! One full-duplex SpaceTerm Protocol connection between a client and a server.
//!
//! - [`carrier`] moves whole frames over one socket. [`unix`] carries account-local IPC, [`wss`]
//!   paired WSS, and [`loopback`] is an in-memory carrier for tests. With the `iroh` feature,
//!   `quic` is the QUIC carrier prototype of #671 beside WSS.
//! - [`io`] runs one I/O thread per connection, waiting on carrier readiness and a wake pipe.
//! - [`outbox`] orders a server's outgoing messages by priority and holds the latest presentation
//!   per channel under one frame of credit.
//! - [`server`] applies the protocol's ordering rules to one admitted connection and routes the
//!   rest to the [`service::PaneService`] or the host's [`service::DirectManagement`].
//! - [`sync`] runs disk syncs and direct-network administration in order on one worker per server.
//! - [`registry`] ends live paired connections on revocation.
//! - [`link`] is the client's [`link::ServerLink`]: concurrent requests, fire-and-forget input,
//!   per-channel delivery, pushed inventory and health.
//! - [`health`] judges liveness from traffic on the live connection.
//!
//! No code here sleeps or polls on a timer. An architecture test enforces this.
pub mod carrier;
pub mod health;
pub mod io;
pub mod link;
pub mod loopback;
pub mod outbox;
#[cfg(feature = "quic")]
pub mod quic;
pub mod registry;
pub mod server;
pub mod service;
pub mod sync;
#[cfg(test)]
mod tests;
pub mod unix;
pub mod wss;

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

#[cfg(test)]
mod architecture_tests {
    /// Connection and carrier code waits only on descriptor readiness and deadlines.
    #[test]
    fn connection_and_carrier_code_never_sleeps() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut sources = Vec::new();
        let mut pending = vec![root.join("connection")];
        while let Some(directory) = pending.pop() {
            for entry in std::fs::read_dir(directory).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                } else if path.extension().is_some_and(|extension| extension == "rs")
                    && path.file_name().is_some_and(|name| name != "tests.rs")
                {
                    sources.push(path);
                }
            }
        }
        // The paired WSS handshakes and pairing messages that precede the carrier.
        sources.push(root.join("direct/wss.rs"));
        assert!(sources.len() > 10);
        let sleep = ["thread", "::sleep"].concat();
        for path in sources {
            let source = std::fs::read_to_string(&path).unwrap();
            assert!(
                !source.contains(&sleep),
                "{} sleeps instead of waiting for readiness",
                path.display()
            );
        }
    }
}
