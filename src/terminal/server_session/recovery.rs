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

    pub fn connected(&mut self, now: Instant) {
        let since = *self.healthy_since.get_or_insert(now);
        if now.saturating_duration_since(since) >= HEALTHY_CONNECTION {
            self.retry();
            self.healthy_since = Some(now);
        }
    }

    /// Returns the retained cause and whether another automatic connection is allowed.
    /// Ordinary transport outages keep their existing recovery behavior. A protocol failure
    /// or unconfirmed shell start bounds the entire episode, including later transport errors.
    pub fn failed(&mut self, error: ServerError) -> (ServerError, bool) {
        self.healthy_since = None;
        if self.cause.is_none() {
            self.cause = self.start.as_ref().and_then(|start| start.cause);
            if error == ServerError::Protocol {
                self.cause.get_or_insert(error);
            }
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
            assert!(recovery.failed(ServerError::Protocol).1);
            recovery.connected(now);
        }
        assert!(!recovery.failed(ServerError::Protocol).1);
        recovery.connected(now);
        recovery.connected(now + Duration::from_secs(5));
        assert!(recovery.failed(ServerError::Protocol).1);
    }
}
