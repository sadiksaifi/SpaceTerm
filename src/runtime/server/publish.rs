//! Pushes each claimed Pane's presentation and Pane events to the channel bound to its lease.
//!
//! A Terminal Session calls its subscription's notifier when it has something new: a screen,
//! an exit, a client effect, hidden input or an accessibility model. The notifier rings the
//! host's wake, and the host's next tick lets [`super::dispatch::Served`] observe each bound
//! channel's Pane under the `PaneService` lock. A screen is captured there as an `Arc` and
//! encoded later on the connection's I/O thread by the channel's own [`ScreenEncoder`]. Nothing
//! here polls.
use super::screen_encoder::{ScreenCapture, ScreenEncoder};
use super::*;
use spaceterm_server::connection::outbox::{
    FrameTicket, Outbox, OutboxError, Presentation, Presented,
};
use spaceterm_server::protocol::{self, Channel, Fatal, PaneEvent};
use std::sync::Mutex;

/// What a Pane holds for the channel bound to a lease.
pub enum Observed {
    Live(Observation),
    /// The lease no longer presents the Pane. `event` is the channel's last, after `last`.
    Gone {
        last: Option<ScreenCapture>,
        event: PaneEvent,
    },
}

/// A live Pane's state, drained from its Terminal Session.
pub struct Observation {
    pub(crate) screen: Option<ScreenCapture>,
    /// Client effects raised under the current focus generation, oldest first.
    pub(crate) effects: Vec<pane::effects::Request>,
    pub(crate) hidden_input: bool,
    /// A changed accessibility revision. A presented screen carries it; otherwise it travels as
    /// a Pane event.
    pub(crate) accessibility: Option<u64>,
}

/// A captured screen waiting for credit. It owns its snapshot and is encoded on the I/O thread.
struct ScreenPresentation {
    capture: ScreenCapture,
    encoder: Arc<Mutex<ScreenEncoder>>,
}

impl Presentation for ScreenPresentation {
    fn encode(self: Box<Self>, ticket: FrameTicket) -> Option<Presented> {
        let update = self
            .encoder
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .encode(&self.capture, ticket.frame, ticket.base)?;
        Some(Presented {
            input: self.capture.screen.input_serial,
            update,
        })
    }
}

/// One bound channel's presentation: its outbox, its encoder, and what it was last given.
pub(super) struct Publication {
    outbox: Outbox,
    channel: Channel,
    visible: bool,
    encoder: Arc<Mutex<ScreenEncoder>>,
    /// The wire screen generation last handed to the outbox while visible.
    presented: Option<u64>,
    hidden_input: Option<bool>,
}

impl Publication {
    /// Opens `channel` on `outbox` for an attached claim. It starts hidden and owes a keyframe,
    /// so a Pane claimed while hidden receives no screen; the client's first `View` shows it.
    pub(super) fn bind(outbox: &Outbox, channel: Channel) -> Result<Self, OutboxError> {
        outbox.open(channel)?;
        outbox.view(channel, false);
        Ok(Self {
            outbox: outbox.clone(),
            channel,
            visible: false,
            encoder: Arc::default(),
            presented: None,
            hidden_input: None,
        })
    }

    /// Applies a client `View`, which the connection already applied to the outbox. A hidden
    /// channel's pending state is dropped there. Revealing presents the current screen again;
    /// the encoder compares it with the acknowledged frame, so an unchanged screen sends nothing.
    pub(super) fn view(&mut self, visible: bool) {
        self.visible = visible;
        if visible {
            self.presented = None;
        }
    }

    /// Pushes what changed. Returns `false` once the channel ended or its connection can take
    /// no more; the caller then forgets the binding.
    pub(super) fn publish(&mut self, observed: Observed) -> bool {
        match observed {
            Observed::Live(observation) => self.live(observation),
            Observed::Gone { last, event } => {
                if let Some(last) = &last {
                    let _ = self.screen(last);
                }
                let _ = self.deliver(self.outbox.end(self.channel, event));
                false
            }
        }
    }

    fn live(&mut self, observation: Observation) -> bool {
        let Observation {
            screen,
            effects,
            hidden_input,
            accessibility,
        } = observation;
        let mut open = true;
        for request in effects {
            open &= self.event(PaneEvent::Effect(request));
        }
        if self.hidden_input != Some(hidden_input) {
            self.hidden_input = Some(hidden_input);
            open &= self.event(PaneEvent::HiddenInput(hidden_input));
        }
        let carried = match &screen {
            Some(screen) => self.screen(screen),
            None => Ok(false),
        };
        open &= carried.is_ok();
        if let Some(revision) = accessibility
            && carried == Ok(false)
        {
            open &= self.event(PaneEvent::Accessibility { revision });
        }
        open
    }

    /// Presents `capture` if the channel is visible and has not been handed this screen.
    /// Returns whether it was presented, or `Err` when the connection can take no more.
    fn screen(&mut self, capture: &ScreenCapture) -> Result<bool, ()> {
        if !self.visible || self.presented == Some(capture.generation) {
            return Ok(false);
        }
        self.presented = Some(capture.generation);
        let presentation = Box::new(ScreenPresentation {
            capture: capture.clone(),
            encoder: Arc::clone(&self.encoder),
        });
        if self.deliver(self.outbox.present(self.channel, presentation)) {
            Ok(true)
        } else {
            Err(())
        }
    }

    fn event(&self, event: PaneEvent) -> bool {
        self.deliver(self.outbox.pane(self.channel, event))
    }

    /// Whether the connection took the message. One this server cannot encode is dropped and
    /// the channel continues. A connection that broke a bound ends with a goodbye.
    fn deliver(&self, result: Result<(), OutboxError>) -> bool {
        match result {
            Ok(()) | Err(OutboxError::Encode(_)) => true,
            Err(OutboxError::Closed) => false,
            Err(error) => {
                let _ = self.outbox.goodbye(Fatal::from(error));
                false
            }
        }
    }
}

/// The last event of a channel whose Pane another lease controls now.
fn takeover(reply: Reply) -> PaneEvent {
    match reply {
        Reply::Takeover {
            session_ended,
            target,
            controller,
        } => PaneEvent::Takeover(protocol::Takeover {
            session_ended,
            target,
            controller,
        }),
        _ => PaneEvent::Failed,
    }
}

impl OwnedPane {
    /// The current screen, captured for a presentation that outlives the lock.
    pub(super) fn capture(&mut self) -> Option<ScreenCapture> {
        let screen = self.screen.as_ref()?;
        Some(ScreenCapture {
            screen: Arc::clone(screen),
            history: self.history.observe(screen),
            generation: self.screen_generation,
            accessibility_revision: self.accessibility.revision(),
        })
    }

    /// Drains the Terminal Session for the controlling channel. Client effects are released
    /// only under a focus generation the client sent.
    pub(super) fn observe(&mut self) -> Observation {
        self.drain();
        let mut effects = Vec::new();
        if self.focus_generation != 0 {
            while let Some(request) = self.effects.next(self.focus_generation) {
                effects.push(request);
            }
        }
        Observation {
            screen: self.capture(),
            effects,
            hidden_input: self.subscription.hidden_input(),
            accessibility: self.accessibility.take_changed_revision(),
        }
    }

    /// Applies a client `View`: the Terminal Session publishes only while visible, at the
    /// declared frame rate.
    pub(super) fn view(&mut self, visible: bool, max_hz: u8) {
        if self.presentable != visible {
            self.presentable = visible;
            self.runtime.set_presentable(visible);
            if !visible {
                self.runtime.end_server_accessibility_demand();
            }
        }
        self.runtime.set_frame_rate(max_hz);
    }
}

impl DevelopmentPaneHost {
    /// What the Pane holds for the channel bound to `lease`, which `device` claimed. An ended
    /// shell yields its final screen once, then the Pane retires.
    pub(super) fn observe(&mut self, device: [u8; 32], lease: &Lease) -> Observed {
        let Some(pane) = &mut self.pane else {
            let event = if self.failed_directory_session == Some((*lease).into()) {
                PaneEvent::DirectoryUnavailable
            } else if self.dormant.is_some() {
                PaneEvent::SessionEnded
            } else {
                PaneEvent::Ended
            };
            return Observed::Gone { last: None, event };
        };
        if pane.lease != *lease || pane.device != Some(device) || !pane.connected {
            let (target, controlling) = (pane.lease.into(), pane.device);
            return Observed::Gone {
                last: None,
                event: takeover(self.takeover(target, controlling, device)),
            };
        }
        let observation = pane.observe();
        if pane.directory_failed {
            return Observed::Gone {
                last: None,
                event: match self.retire_exited() {
                    Ok(()) => PaneEvent::DirectoryUnavailable,
                    Err(_) => PaneEvent::Failed,
                },
            };
        }
        if pane.failed {
            return Observed::Gone {
                last: observation.screen,
                event: PaneEvent::Failed,
            };
        }
        if pane.ended {
            let event = match self.retire_exited() {
                Ok(()) => PaneEvent::Ended,
                Err(_) => PaneEvent::Failed,
            };
            self.pane = None;
            return Observed::Gone {
                last: observation.screen,
                event,
            };
        }
        Observed::Live(observation)
    }

    /// Applies a client `View` to the Pane `lease` controls.
    pub(super) fn view(&mut self, lease: &Lease, visible: bool, max_hz: u8) {
        if let Some(pane) = &mut self.pane
            && pane.lease == *lease
        {
            pane.view(visible, max_hz);
        }
    }

    /// Marks the end of Input `serial` on the Pane `lease` controls.
    pub(super) fn input_written(&mut self, lease: &Lease, serial: u32) {
        if let Some(pane) = &self.pane
            && pane.lease == *lease
        {
            pane.runtime.note_input_serial(serial);
        }
    }
}

#[cfg(test)]
#[path = "publish_tests.rs"]
mod tests;
