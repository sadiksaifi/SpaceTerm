//! Bounded protocol recovery and conservative resolution of a possibly submitted shell start.
use super::*;
use std::time::Instant;

const AUTOMATIC_RECONNECTIONS: u8 = 3;
const HEALTHY_CONNECTION: Duration = Duration::from_secs(5);

struct Start {
    boot: [u8; 32],
    cause: Option<ServerError>,
}

#[derive(Default)]
pub(super) struct Recovery {
    failures: u8,
    cause: Option<ServerError>,
    healthy_since: Option<Instant>,
    observed_boot: Option<[u8; 32]>,
    start: Option<Start>,
}

impl Recovery {
    pub fn start_unconfirmed(&self) -> bool {
        self.start.is_some()
    }

    pub fn retry(&mut self) {
        self.failures = 0;
        self.cause = None;
        self.healthy_since = None;
    }

    pub fn direct_reachable(&mut self) -> bool {
        if self.cause == Some(ServerError::Direct(DirectError::Transport)) {
            self.retry();
            true
        } else {
            false
        }
    }

    /// Starts, or keeps, the healthy period of a live connection.
    pub fn connected(&mut self, now: Instant) {
        let since = *self.healthy_since.get_or_insert(now);
        if now.saturating_duration_since(since) >= HEALTHY_CONNECTION {
            self.retry();
            self.healthy_since = Some(now);
        }
    }

    /// Returns the retained cause and whether another automatic connection is allowed.
    /// Account-local transport outages keep their existing recovery behavior. A paired transport
    /// outage, protocol failure or unconfirmed shell start bounds the entire episode. A confirmed
    /// paired connection renews a transport outage, but never a protocol failure.
    /// A connection that stayed healthy for [`HEALTHY_CONNECTION`] before failing at `now`
    /// renews recovery first, so an idle Pane whose worker never turned is judged by time.
    pub fn failed(&mut self, error: ServerError, now: Instant) -> (ServerError, bool) {
        if self
            .healthy_since
            .take()
            .is_some_and(|since| now.saturating_duration_since(since) >= HEALTHY_CONNECTION)
        {
            self.retry();
        }
        if self.cause.is_none() {
            self.cause = self.start.as_ref().and_then(|start| start.cause);
            if matches!(
                error,
                ServerError::Protocol | ServerError::Direct(DirectError::Transport)
            ) {
                self.cause.get_or_insert(error);
            }
        }
        // Once protocol recovery begins, another inventory is not proof of protocol health.
        if error == ServerError::Protocol
            && self.cause == Some(ServerError::Direct(DirectError::Transport))
        {
            self.cause = Some(error);
        }
        if let Some(cause) = self.cause {
            self.failures = self.failures.saturating_add(1);
            (cause, self.failures <= AUTOMATIC_RECONNECTIONS)
        } else {
            (error, true)
        }
    }

    pub fn begin_start(&mut self, boot: Option<[u8; 32]>) -> Result<(), ServerError> {
        if let Some(start) = &self.start {
            return Err(start.cause.unwrap_or(ServerError::Protocol));
        }
        self.start = Some(Start {
            boot: boot.or(self.observed_boot).ok_or(ServerError::Protocol)?,
            cause: None,
        });
        Ok(())
    }

    /// A reply from the original request settles it. A failed exchange may have submitted it.
    pub fn started(&mut self, result: &Result<Reply, ServerError>) {
        match result {
            Ok(Reply::Attached { .. } | Reply::Restored { .. } | Reply::Takeover { .. }) => {
                self.start = None;
            }
            Err(error) if !matches!(error, ServerError::Protocol | ServerError::NotRunning) => {
                self.start = None;
            }
            _ => {
                if let Some(start) = &mut self.start {
                    start.cause = Some(match result {
                        Err(error) => *error,
                        _ => ServerError::Protocol,
                    });
                }
            }
        }
    }

    /// A same-boot dormant observation can overtake the original request on its old transport.
    /// Only a live grant or proof that the original server run ended unlocks another start.
    pub fn observed(&mut self, reply: &Reply) -> Result<(), ServerError> {
        let boot = match reply {
            Reply::Attached { lease } => {
                self.start = None;
                Some(lease.server)
            }
            Reply::Restored { server, .. } => Some(*server),
            Reply::Takeover { target, .. } => Some(target.server),
            _ => return Err(ServerError::Protocol),
        };
        self.observed_boot = boot;
        if let Some(start) = &self.start {
            if Some(start.boot) != boot {
                self.start = None;
            } else {
                return Err(start.cause.unwrap_or(ServerError::Protocol));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stable_connection_renews_recovery_but_a_brief_screen_does_not() {
        let mut recovery = Recovery::default();
        let now = Instant::now();
        for _ in 0..3 {
            assert!(recovery.failed(ServerError::Protocol, now).1);
            recovery.connected(now);
        }
        assert!(!recovery.failed(ServerError::Protocol, now).1);
        recovery.connected(now);
        recovery.connected(now + Duration::from_secs(5));
        assert!(recovery.failed(ServerError::Protocol, now).1);
    }

    /// An idle Pane's worker marks its connection healthy once, when it attaches, and never
    /// again. Each drop after more than 5 s of health still allows another reconnection.
    #[test]
    fn an_idle_connection_that_stayed_healthy_renews_recovery_when_it_fails() {
        let mut recovery = Recovery::default();
        let mut now = Instant::now();
        for drop in 0..10 {
            recovery.connected(now);
            now += Duration::from_secs(6);
            assert!(
                recovery.failed(ServerError::Protocol, now).1,
                "drop {drop} after a healthy connection was refused"
            );
            now += Duration::from_secs(1);
        }
    }

    #[test]
    fn a_connection_that_fails_within_5_seconds_does_not_renew_recovery() {
        let mut recovery = Recovery::default();
        let mut now = Instant::now();
        for _ in 0..3 {
            recovery.connected(now);
            now += Duration::from_secs(4);
            assert!(recovery.failed(ServerError::Protocol, now).1);
        }
        recovery.connected(now);
        assert!(
            !recovery
                .failed(ServerError::Protocol, now + Duration::from_secs(4))
                .1
        );
    }
}
