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
    /// Whether the outstanding ping was already reported by `suspect`.
    suspected: bool,
}

impl Liveness {
    pub fn new(now: Instant) -> Self {
        Self {
            received: now,
            pinged: None,
            blocked: None,
            nonce: 0,
            suspected: false,
        }
    }

    /// Records any whole frame from the peer.
    pub fn received(&mut self, now: Instant) {
        self.received = now;
        self.pinged = None;
    }

    /// Records a write attempt. `progressed` is whether any byte reached the carrier or the
    /// socket; `pending` is whether bytes still wait, in the carrier or the outbox. Progress
    /// restarts the stall timer, so a large backlog that keeps draining is not a stall.
    pub fn wrote(&mut self, now: Instant, progressed: bool, pending: bool) {
        self.blocked = match (pending, progressed, self.blocked) {
            (false, _, _) => None,
            (true, true, _) | (true, false, None) => Some(now),
            (true, false, Some(since)) => Some(since),
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
            self.suspected = false;
            self.nonce = self.nonce.wrapping_add(1);
            return Check::Ping {
                nonce: self.nonce,
                next: next.min(now + limits::PING_AFTER),
            };
        }
        if let Some(pinged) = self.pinged
            && !self.suspected
        {
            next = next.min(pinged + limits::SUSPECT_AFTER);
        }
        Check::Live {
            next: next.min(last + limits::PING_AFTER),
        }
    }

    /// Whether a ping has gone unanswered for [`limits::SUSPECT_AFTER`]. True once per ping.
    pub fn suspect(&mut self, now: Instant) -> bool {
        let unanswered = self
            .pinged
            .is_some_and(|pinged| now.duration_since(pinged) >= limits::SUSPECT_AFTER);
        if unanswered && !self.suspected {
            self.suspected = true;
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn an_unanswered_ping_makes_the_path_suspect_once_before_the_connection_is_dead() {
        let start = Instant::now();
        let mut liveness = Liveness::new(start);
        let ping = start + limits::PING_AFTER;
        assert!(matches!(liveness.check(ping, None), Check::Ping { .. }));
        assert!(!liveness.suspect(ping + limits::SUSPECT_AFTER / 2));
        let Check::Live { next } = liveness.check(ping + Duration::from_millis(1), None) else {
            panic!("expected a live connection");
        };
        assert_eq!(
            next,
            ping + limits::SUSPECT_AFTER,
            "the thread wakes to suspect"
        );
        assert!(liveness.suspect(ping + limits::SUSPECT_AFTER));
        assert!(
            !liveness.suspect(ping + limits::SUSPECT_AFTER * 2),
            "once per ping"
        );
        assert!(limits::PING_AFTER + limits::SUSPECT_AFTER < limits::DEAD_AFTER);
        liveness.received(ping + limits::SUSPECT_AFTER * 2);
        assert!(!liveness.suspect(ping + limits::SUSPECT_AFTER * 3));
    }

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
        // A backlog that keeps draining is not a stall.
        for second in 15..30 {
            liveness.received(start + Duration::from_secs(second));
            liveness.wrote(start + Duration::from_secs(second), true, true);
        }
        assert!(matches!(
            liveness.check(start + Duration::from_secs(30), None),
            Check::Live { .. }
        ));
        liveness.wrote(start + Duration::from_secs(30), false, true);
        liveness.wrote(start + Duration::from_secs(31), false, true);
        liveness.received(start + Duration::from_secs(39));
        assert_eq!(
            liveness.check(start + Duration::from_secs(40), None),
            Check::Dead(Death::Stalled)
        );
        liveness.wrote(start + Duration::from_secs(41), true, false);
        liveness.received(start + Duration::from_secs(41));
        assert!(matches!(
            liveness.check(start + Duration::from_secs(42), Some(start + Duration::from_secs(35))),
            Check::Live { next } if next == start + Duration::from_secs(45)
        ));
    }
}
