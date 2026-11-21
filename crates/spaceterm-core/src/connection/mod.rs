//! One full-duplex SpaceTerm Protocol connection between a client and a server.
//!
//! - [`carrier`] moves whole frames over one connection. [`unix`] carries account-local IPC,
//!   [`quic`] paired connections, and [`loopback`] is an in-memory carrier for tests.
//! - [`spaceterm_client::connection::path`] tells a paired client when the system would reach its server from another local
//!   address, so its QUIC socket moves there.
//! - [`route_signal`] turns readable when the system's routes or addresses change.
//! - [`io`] runs one I/O thread per connection, waiting on carrier readiness and a wake pipe.
//! - [`outbox`] orders a server's outgoing messages by priority and holds the latest presentation
//!   per channel under one frame of credit.
//! - [`server`] applies the protocol's ordering rules to one admitted connection and routes the
//!   rest to the [`service::PaneService`] or the host's [`service::DirectManagement`].
//! - [`sync`] runs disk syncs and direct-network administration in order on one worker per server.
//! - [`registry`] ends live paired connections on revocation.
//! - [`link`] is the client's [`link::CoreLink`]: concurrent requests, fire-and-forget input,
//!   per-channel delivery, pushed inventory and health.
//! - [`health`] judges liveness from traffic on the live connection.
//!
//! No code here sleeps or polls on a timer. An architecture test enforces this.
pub mod admitted;
pub mod io;
pub mod outbox;
pub mod registry;
pub mod route_signal;
pub mod service;
pub mod sync;
#[cfg(test)]
mod tests;
pub mod unix;

pub use spaceterm_client::connection::{Wake, carrier, health, link, loopback, quic};

#[cfg(test)]
mod architecture_tests {
    /// Connection and carrier code waits only on descriptor readiness and deadlines.
    #[test]
    fn connection_and_carrier_code_never_sleeps() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut sources = Vec::new();
        let client = root.join("../../spaceterm-client/src");
        let mut pending = vec![root.join("connection"), client.join("connection")];
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

        sources.push(client.join("direct/quic.rs"));
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
