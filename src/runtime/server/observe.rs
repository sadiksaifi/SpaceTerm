//! Observer channels: any client may watch a Pane's running Terminal Session without control.
//!
//! An observer receives frames, line state and progress, and reads Scrollback. It never claims,
//! resizes, starts a shell, focuses, or receives effects or accessibility, so the Controlling
//! Client's presentation is unchanged. A visible observer only makes the Terminal Session publish
//! frames while its controller hides it.
use super::publish::{Observation, Observed};
use super::*;
use spaceterm_server::protocol::{ObserveReply, PaneEvent, SessionState, screen::History};

impl OwnedPane {
    /// The Terminal Session publishes frames while its controller or an observer shows it.
    pub(super) fn apply_presentable(&self) {
        self.runtime
            .set_presentable(self.presentable || self.observed);
    }

    pub(super) fn set_observed(&mut self, observed: bool) {
        if self.observed != observed {
            self.observed = observed;
            self.apply_presentable();
        }
    }

    /// Drains the Terminal Session for an observer. Effects and accessibility stay with the
    /// controlling channel.
    fn observe_without_control(&mut self) -> Observation {
        self.drain();
        Observation {
            screen: self.capture(),
            effects: Vec::new(),
            line: self.subscription.line(),
            accessibility: None,
            progress: self.subscription.progress(),
        }
    }

    fn history(&self) -> History {
        self.screen
            .as_ref()
            .map(|screen| History {
                epoch: screen.history.epoch,
                oldest: screen.history.oldest,
                total: screen.history.total,
            })
            .unwrap_or_default()
    }

    fn running(&self) -> bool {
        !self.ended && !self.failed
    }
}

impl DevelopmentPaneHost {
    /// Starts an observation of this Pane's running Terminal Session. `fence` refuses another
    /// running Session as `Stale`. A Pane where nothing runs says why, fenced or not.
    pub(super) fn observe_start(
        &mut self,
        fence: Option<[u8; 32]>,
    ) -> Result<ObserveReply, Refusal> {
        if let Some(pane) = &mut self.pane {
            pane.drain();
        }
        let Some(pane) = self.pane.as_ref().filter(|pane| pane.running()) else {
            return Ok(ObserveReply::NoSession {
                state: match self.dormant {
                    Some(dormant) => SessionState::Ended(dormant.end),
                    None => SessionState::NotStarted,
                },
            });
        };
        if fence.is_some_and(|session| session != pane.lease.session) {
            return Err(Refusal::Stale);
        }
        Ok(ObserveReply::Observing {
            session: pane.lease.session,
            history: pane.history(),
        })
    }

    /// Whether `session` is still this Pane's running Terminal Session.
    pub(super) fn observer_live(&self, session: [u8; 32]) -> bool {
        self.pane
            .as_ref()
            .is_some_and(|pane| pane.lease.session == session)
    }

    /// What the Pane holds for an observer of `session`. The observer never retires the Pane;
    /// its controller or the host's timed work does.
    pub(super) fn observe_session(&mut self, session: [u8; 32]) -> Observed {
        let Some(pane) = self
            .pane
            .as_mut()
            .filter(|pane| pane.lease.session == session)
        else {
            return Observed::Gone {
                last: None,
                event: if self.dormant.is_some() {
                    PaneEvent::SessionEnded
                } else {
                    PaneEvent::Ended
                },
            };
        };
        let observation = pane.observe_without_control();
        if pane.failed || pane.directory_failed {
            return Observed::Gone {
                last: observation.screen,
                event: PaneEvent::Failed,
            };
        }
        if pane.ended {
            return Observed::Gone {
                last: observation.screen,
                event: match pane.end {
                    Some(_) => PaneEvent::SessionEnded,
                    None => PaneEvent::Ended,
                },
            };
        }
        Observed::Live(observation)
    }

    /// Reads a Scrollback page of `session` for an observer.
    pub(super) fn observer_scrollback_page(
        &mut self,
        session: [u8; 32],
        read: ScrollbackRead,
        reply: PageReply,
    ) -> Result<(), Refusal> {
        self.pane
            .as_mut()
            .filter(|pane| pane.lease.session == session)
            .ok_or(Refusal::Stale)?
            .scrollback_page(read, reply)
    }

    /// Whether a visible observer shows `session`.
    pub(super) fn set_observed(&mut self, session: [u8; 32], observed: bool) {
        if let Some(pane) = self
            .pane
            .as_mut()
            .filter(|pane| pane.lease.session == session)
        {
            pane.set_observed(observed);
        }
    }
}
