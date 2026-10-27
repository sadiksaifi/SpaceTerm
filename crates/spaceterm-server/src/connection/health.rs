//! Liveness judged from traffic on the live connection, never from extra connections.
//!
//! Either peer pings after [`limits::PING_AFTER`] with nothing received. A connection is dead
//! after [`limits::DEAD_AFTER`] with nothing received, or after [`limits::STALL_AFTER`] of writes
//! or frame acknowledgements that make no progress. Callers pass `now`, so tests need no clock.
use crate::protocol::limits;
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Death {
    /// Nothing arrived for [`limits::DEAD_AFTER`].
    Silent,
    /// Queued writes or an unacknowledged frame made no progress for [`limits::STALL_AFTER`].
    Stalled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Check {
    /// Check again at `next` at the latest.
    Live {
        next: Instant,
    },
    /// Send `Ping { nonce }` now, then check again at `next` at the latest.
    Ping {
        nonce: u64,
        next: Instant,
    },
    Dead(Death),
}

#[derive(Debug)]
pub struct Liveness {
    received: Instant,
    pinged: Option<Instant>,
    blocked: Option<Instant>,
    nonce: u64,
}

impl Liveness {
    pub fn new(now: Instant) -> Self {
        Self {
            received: now,
            pinged: None,
            blocked: None,
            nonce: 0,
        }
    }

    /// Records any whole frame from the peer.
    pub fn received(&mut self, now: Instant) {
        self.received = now;
        self.pinged = None;
    }

    /// Records whether queued bytes are waiting on the carrier after a write attempt.
    pub fn wrote(&mut self, now: Instant, blocked: bool) {
        self.blocked = match (blocked, self.blocked) {
            (false, _) => None,
            (true, Some(since)) => Some(since),
            (true, None) => Some(now),
        };
    }

    /// `unacknowledged` is when the oldest frame still awaiting acknowledgement was written.
    pub fn check(&mut self, now: Instant, unacknowledged: Option<Instant>) -> Check {
        let stall = [self.blocked, unacknowledged].into_iter().flatten().min();
        if stall.is_some_and(|since| now.duration_since(since) >= limits::STALL_AFTER) {
            return Check::Dead(Death::Stalled);
        }
        let silent = now.duration_since(self.received);
        if silent >= limits::DEAD_AFTER {
            return Check::Dead(Death::Silent);
        }
        let mut next = self.received + limits::DEAD_AFTER;
        if let Some(since) = stall {
            next = next.min(since + limits::STALL_AFTER);
        }
        let last = self.pinged.unwrap_or(self.received);
        if now.duration_since(last) >= limits::PING_AFTER {
            self.pinged = Some(now);
            self.nonce = self.nonce.wrapping_add(1);
            return Check::Ping {
                nonce: self.nonce,
                next: next.min(now + limits::PING_AFTER),
            };
        }
        Check::Live {
            next: next.min(last + limits::PING_AFTER),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_quiet_connection_is_pinged_then_declared_silent() {
        let start = Instant::now();
        let mut liveness = Liveness::new(start);
        assert_eq!(
            liveness.check(start, None),
            Check::Live {
                next: start + limits::PING_AFTER
            }
        );
        let Check::Ping { nonce: 1, next } = liveness.check(start + limits::PING_AFTER, None)
        else {
            panic!("expected a ping");
        };
        assert_eq!(next, start + limits::PING_AFTER * 2);
        assert!(matches!(
            liveness.check(start + limits::PING_AFTER * 2, None),
            Check::Ping { nonce: 2, .. }
        ));
        assert_eq!(
            liveness.check(start + limits::DEAD_AFTER, None),
            Check::Dead(Death::Silent)
        );
    }

    #[test]
    fn traffic_renews_liveness_and_stalls_are_judged_separately() {
        let start = Instant::now();
        let mut liveness = Liveness::new(start);
        liveness.received(start + Duration::from_secs(14));
        assert!(matches!(
            liveness.check(start + Duration::from_secs(15), None),
            Check::Live { .. }
        ));
        liveness.wrote(start + Duration::from_secs(15), true);
        liveness.wrote(start + Duration::from_secs(16), true);
        liveness.received(start + Duration::from_secs(24));
        assert_eq!(
            liveness.check(start + Duration::from_secs(25), None),
            Check::Dead(Death::Stalled)
        );
        liveness.wrote(start + Duration::from_secs(25), false);
        assert!(matches!(
            liveness.check(start + Duration::from_secs(25), Some(start + Duration::from_secs(20))),
            Check::Live { next } if next == start + Duration::from_secs(29)
        ));
    }
}
