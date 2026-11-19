//! Bounded protocol recovery and conservative resolution of a possibly submitted shell start.
use super::*;
use std::time::Instant;

const AUTOMATIC_RECONNECTIONS: u8 = 3;
const HEALTHY_CONNECTION: Duration = Duration::from_secs(5);

struct Start {
    boot: [u8; 32],
    cause: Option<CoreError>,
    /// A background start, which grants control to nobody. Seeing its exact Pane run a Session
    /// in the same Core run with nobody in control proves it started.
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
    /// Only a live grant, proof that the original server run ended, or for a background start
    /// its Pane `pane` running a Session in that run with nobody in control unlocks another
    /// start. Any validated observation ends a restart wait, also for a Pane that stays dormant.
    pub fn observed(&mut self, reply: &Reply, pane: [u8; 32]) -> Result<(), CoreError> {
        let boot = match reply {
            Reply::Attached { lease } => {
                self.start = None;
                Some(lease.core)
            }
            Reply::Restored { core, .. } => Some(*core),
            Reply::Takeover {
                target, controller, ..
            } => {
                let live_and_unowned = target.session != pane
                    && *controller == spaceterm_core::pane::control::Controller::Nobody;
                if live_and_unowned
                    && self
                        .start
                        .as_ref()
                        .is_some_and(|start| start.background && start.boot == target.core)
                {
                    self.start = None;
                }
                Some(target.core)
            }
            _ => return Err(CoreError::Protocol),
        };
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
        recovery.observed(&restored, [2; 32]).unwrap();
        // The connection drops again after the first wait's deadline.
        let later = now + Duration::from_secs(200);
        assert!(!recovery.restart_expired(later));
        assert!(recovery.failed(CoreError::NotRunning, later).1);
        assert!(
            recovery.restart_wait(later),
            "a new restart gets its own wait"
        );
    }

    /// A background start whose reply was lost settles once the same Core run shows its Pane
    /// running a Session nobody controls. A dormant observation in that run, or one naming a
    /// controller, keeps the guard, and so does any such observation of an ordinary start.
    #[test]
    fn a_lost_background_start_settles_on_its_live_unowned_session() {
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

        let mut recovery = lost(true);
        assert!(
            recovery
                .observed(&takeover(pane, Controller::Nobody), pane)
                .is_err()
        );
        assert!(
            recovery
                .observed(
                    &takeover(
                        [9; 32],
                        Controller::Client {
                            name: "Other".try_into().unwrap(),
                            this_client: false,
                        },
                    ),
                    pane,
                )
                .is_err(),
            "another client's Session proves nothing about this start"
        );
        recovery
            .observed(&takeover([9; 32], Controller::Nobody), pane)
            .unwrap();
        assert!(!recovery.start_unconfirmed());

        let mut ordinary = lost(false);
        assert!(
            ordinary
                .observed(&takeover([9; 32], Controller::Nobody), pane)
                .is_err()
        );
        assert!(ordinary.start_unconfirmed());
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
