//! Bounded protocol recovery and conservative resolution of a possibly submitted shell start.
use super::*;
use std::time::Instant;

const AUTOMATIC_RECONNECTIONS: u8 = 3;
const HEALTHY_CONNECTION: Duration = Duration::from_secs(5);

struct Start {
    boot: [u8; 32],
    cause: Option<CoreError>,
    /// A background start, which grants control to nobody. SpaceTerm Core starts a Pane in the
    /// background at most once per run, and only while it has no live Session and none that
    /// ended in that run, so any observation of its exact Pane in the same run settles it.
    background: bool,
}

#[derive(Default)]
pub(super) struct Recovery {
    failures: u8,
    cause: Option<CoreError>,
    healthy_since: Option<Instant>,
    observed_boot: Option<[u8; 32]>,
    start: Option<Start>,
    restart_since: Option<Instant>,
}

impl Recovery {
    pub fn restart_wait(&mut self, now: Instant) -> bool {
        now.saturating_duration_since(*self.restart_since.get_or_insert(now))
            < super::restart::RECONNECT_LIMIT
    }

    pub fn restart_expired(&self, now: Instant) -> bool {
        self.restart_since.is_some_and(|since| {
            now.saturating_duration_since(since) >= super::restart::RECONNECT_LIMIT
        })
    }

    pub fn start_unconfirmed(&self) -> bool {
        self.start.is_some()
    }

    fn reset_failures(&mut self) {
        self.failures = 0;
        self.cause = None;
        self.healthy_since = None;
    }
    pub fn retry(&mut self) {
        self.reset_failures();
        self.restart_since = None;
    }

    pub fn direct_reachable(&mut self) -> bool {
        if self.cause == Some(CoreError::Direct(DirectError::Transport)) {
            self.reset_failures();
            true
        } else {
            false
        }
    }

    /// Starts, or keeps, the healthy period of a live connection.
    pub fn connected(&mut self, now: Instant) {
        self.restart_since = None;
        let since = *self.healthy_since.get_or_insert(now);
        if now.saturating_duration_since(since) >= HEALTHY_CONNECTION {
            self.reset_failures();
            self.healthy_since = Some(now);
        }
    }

    /// Returns the retained cause and whether another automatic connection is allowed.
    /// Account-local transport outages keep their existing recovery behavior. A paired transport
    /// outage, protocol failure or unconfirmed shell start bounds the entire episode. A confirmed
    /// paired connection renews a transport outage, but never a protocol failure.
    /// A connection that stayed healthy for [`HEALTHY_CONNECTION`] before failing at `now`
    /// renews recovery first, so an idle Pane whose worker never turned is judged by time.
    pub fn failed(&mut self, error: CoreError, now: Instant) -> (CoreError, bool) {
        if self
            .healthy_since
            .take()
            .is_some_and(|since| now.saturating_duration_since(since) >= HEALTHY_CONNECTION)
        {
            self.reset_failures();
        }
        if self.cause.is_none() {
            self.cause = self.start.as_ref().and_then(|start| start.cause);
            if matches!(
                error,
                CoreError::Protocol | CoreError::Direct(DirectError::Transport)
            ) {
                self.cause.get_or_insert(error);
            }
        }
        // Once protocol recovery begins, another inventory is not proof of protocol health.
        if error == CoreError::Protocol
            && self.cause == Some(CoreError::Direct(DirectError::Transport))
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

    pub fn begin_start(
        &mut self,
        boot: Option<[u8; 32]>,
        background: bool,
    ) -> Result<(), CoreError> {
        if let Some(start) = &self.start {
            return Err(start.cause.unwrap_or(CoreError::Protocol));
        }
        self.start = Some(Start {
            boot: boot.or(self.observed_boot).ok_or(CoreError::Protocol)?,
            cause: None,
            background,
        });
        Ok(())
    }

    /// A reply from the original request settles it. A failed exchange may have submitted it.
    pub fn started(&mut self, result: &Result<Reply, CoreError>) {
        match result {
            Ok(Reply::Attached { .. } | Reply::Restored { .. } | Reply::Takeover { .. }) => {
                self.start = None;
            }
            Err(error)
                if !matches!(
                    error,
                    CoreError::Protocol
                        | CoreError::NotRunning
                        | CoreError::Direct(DirectError::Transport)
                ) =>
            {
                self.start = None;
            }
            _ => {
                if let Some(start) = &mut self.start {
                    start.cause = Some(match result {
                        Err(error) => *error,
                        _ => CoreError::Protocol,
                    });
                }
            }
        }
    }

    /// A same-boot dormant observation can overtake the original request on its old transport.
    /// Only a live grant or proof that the original server run ended unlocks another ordinary
    /// start. A background start settles on any observation of its Pane in the same run,
    /// whoever controls it: a delayed Prepare that arrives after it only observes, and asking
    /// again is safe for the same reason. Any validated observation ends a restart wait, also
    /// for a Pane that stays dormant.
    pub fn observed(&mut self, reply: &Reply) -> Result<(), CoreError> {
        let boot = match reply {
            Reply::Attached { lease } => {
                self.start = None;
                Some(lease.core)
            }
            Reply::Restored { core, .. } => Some(*core),
            Reply::Takeover { target, .. } => Some(target.core),
            _ => return Err(CoreError::Protocol),
        };
        if self
            .start
            .as_ref()
            .is_some_and(|start| start.background && Some(start.boot) == boot)
        {
            self.start = None;
        }
        self.observed_boot = boot;
        if let Some(start) = &self.start {
            if Some(start.boot) != boot {
                self.start = None;
            } else {
                return Err(start.cause.unwrap_or(CoreError::Protocol));
            }
        }
        self.restart_since = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_restart_wait_has_one_deadline_even_when_each_attempt_fails() {
        let mut recovery = Recovery::default();
        let now = Instant::now();
        assert!(recovery.restart_wait(now));
        for seconds in [1, 5, 60, 179] {
            recovery.failed(CoreError::ServiceBusy, now + Duration::from_secs(seconds));
            assert!(recovery.restart_wait(now + Duration::from_secs(seconds)));
        }
        assert!(!recovery.restart_wait(now + Duration::from_secs(180)));
    }

    #[test]
    fn a_dormant_reconnect_ends_the_restart_wait() {
        let mut recovery = Recovery::default();
        let now = Instant::now();
        assert!(recovery.restart_wait(now));
        let restored = Reply::Restored {
            fresh: false,
            core: [7; 32],
            pane: [2; 32],
            workspace: [3; 32],
            tab: [4; 32],
            recovery: None,
            directory_unavailable: false,
            session_ended: None,
        };
        recovery.observed(&restored).unwrap();
        // The connection drops again after the first wait's deadline.
        let later = now + Duration::from_secs(200);
        assert!(!recovery.restart_expired(later));
        assert!(recovery.failed(CoreError::NotRunning, later).1);
        assert!(
            recovery.restart_wait(later),
            "a new restart gets its own wait"
        );
    }

    /// A background start whose reply was lost settles on any observation of its Pane in the
    /// same Core run, whoever controls it: a dormant one, its own unowned Session, or a Session
    /// another client acquired first. An observation of another run's Pane keeps nothing, and
    /// an ordinary start keeps its guard on every same-run observation but a live grant.
    #[test]
    fn a_lost_background_start_settles_on_any_observation_in_its_run() {
        use spaceterm_core::pane::control::Controller;
        let pane = [2; 32];
        let takeover = |session: [u8; 32], controller: Controller| Reply::Takeover {
            target: spaceterm_core::pane::PaneTarget {
                core: [7; 32],
                session,
            },
            controller,
            session_ended: None,
        };
        let lost = |background: bool| {
            let mut recovery = Recovery::default();
            recovery.begin_start(Some([7; 32]), background).unwrap();
            recovery.started(&Err(CoreError::Direct(DirectError::Transport)));
            recovery
        };
        let other = || Controller::Client {
            name: "Other".into(),
            this_client: false,
        };
        for observation in [
            takeover(pane, Controller::Nobody),
            takeover([9; 32], Controller::Nobody),
            takeover([9; 32], other()),
        ] {
            let mut recovery = lost(true);
            recovery.observed(&observation).unwrap();
            assert!(!recovery.start_unconfirmed(), "{observation:?}");
        }
        for observation in [takeover([9; 32], Controller::Nobody), takeover([9; 32], other())] {
            let mut ordinary = lost(false);
            assert!(ordinary.observed(&observation).is_err());
            assert!(ordinary.start_unconfirmed());
        }
    }

    #[test]
    fn a_stable_connection_renews_recovery_but_a_brief_screen_does_not() {
        let mut recovery = Recovery::default();
        let now = Instant::now();
        for _ in 0..3 {
            assert!(recovery.failed(CoreError::Protocol, now).1);
            recovery.connected(now);
        }
        assert!(!recovery.failed(CoreError::Protocol, now).1);
        recovery.connected(now);
        recovery.connected(now + Duration::from_secs(5));
        assert!(recovery.failed(CoreError::Protocol, now).1);
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
                recovery.failed(CoreError::Protocol, now).1,
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
            assert!(recovery.failed(CoreError::Protocol, now).1);
        }
        recovery.connected(now);
        assert!(
            !recovery
                .failed(CoreError::Protocol, now + Duration::from_secs(4))
                .1
        );
    }
}
