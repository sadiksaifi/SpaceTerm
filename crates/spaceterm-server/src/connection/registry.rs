//! Live paired connections, so revocation reaches them at once.
//!
//! The host's direct-network state owns one registry and changes it under its own lock. Every
//! paired connection registers after admission and unregisters when it ends. Revoking a pairing,
//! disabling direct access and an uncertain commit each end the affected connections: the registry
//! queues `Goodbye(Revoked)`, which also makes the connection stop dispatching, and severs its
//! receiving, so no further request arrives. The I/O thread then writes the goodbye and
//! closes the carrier.
use super::outbox::Outbox;
use super::service::Pairing;
use crate::direct::PairingId;
use crate::protocol::Fatal;

/// Stops a connection from receiving and wakes its I/O thread. A QUIC carrier marks itself
/// severed and rings its bell; writing continues, so a queued goodbye still leaves.
pub trait Sever: Send + Sync {
    fn sever(&self);
}

struct Entry {
    connection: [u8; 32],
    pairing: Pairing,
    outbox: Outbox,
    sever: Box<dyn Sever>,
}

#[derive(Default)]
pub struct PairedConnections {
    entries: Vec<Entry>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("paired connections reached capacity")]
pub struct Full;

/// The most paired connections one server holds at once.
pub const MAX_PAIRED_CONNECTIONS: usize = 64;

impl PairedConnections {
    pub fn register(
        &mut self,
        connection: [u8; 32],
        pairing: Pairing,
        outbox: Outbox,
        sever: Box<dyn Sever>,
    ) -> Result<(), Full> {
        if self.entries.len() >= MAX_PAIRED_CONNECTIONS {
            return Err(Full);
        }
        self.entries.push(Entry {
            connection,
            pairing,
            outbox,
            sever,
        });
        Ok(())
    }

    pub fn unregister(&mut self, connection: [u8; 32]) {
        self.entries.retain(|entry| entry.connection != connection);
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Ends every connection of a revoked pairing.
    pub fn revoke(&mut self, pairing: PairingId) {
        self.end(|entry| entry.pairing.pairing == pairing);
    }

    /// Ends every connection admitted before the current listener generation, after direct access
    /// is disabled or a commit becomes uncertain.
    pub fn retire_generation(&mut self, current: [u8; 32]) {
        self.end(|entry| entry.pairing.generation != current);
    }

    /// Ends every paired connection.
    pub fn end_all(&mut self) {
        self.end(|_| true);
    }

    fn end(&mut self, matches: impl Fn(&Entry) -> bool) {
        self.entries.retain(|entry| {
            if !matches(entry) {
                return true;
            }
            let _ = entry.outbox.goodbye(Fatal::Revoked);
            entry.sever.sever();
            false
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Count(Arc<AtomicUsize>);
    impl Sever for Count {
        fn sever(&self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn revocation_and_retired_generations_end_only_their_connections() {
        let severed = Arc::new(AtomicUsize::new(0));
        let mut registry = PairedConnections::default();
        let outboxes: Vec<_> = (0..3).map(|_| Outbox::new(Arc::new(|| {}))).collect();
        let pairings = [([1; 32], [7; 32]), ([2; 32], [7; 32]), ([2; 32], [8; 32])];
        for (index, (pairing, generation)) in pairings.into_iter().enumerate() {
            registry
                .register(
                    [index as u8; 32],
                    Pairing {
                        pairing,
                        generation,
                    },
                    outboxes[index].clone(),
                    Box::new(Count(Arc::clone(&severed))),
                )
                .unwrap();
        }
        registry.retire_generation([8; 32]);
        assert_eq!(registry.len(), 1);
        assert!(outboxes[0].is_closing() && outboxes[1].is_closing());
        assert!(!outboxes[2].is_closing());
        registry.revoke([2; 32]);
        assert!(registry.is_empty() && outboxes[2].is_closing());
        assert_eq!(severed.load(Ordering::Relaxed), 3);
    }
}
