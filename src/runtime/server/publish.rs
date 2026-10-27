//! Pushes a claimed Pane's presentation and Pane events to the connection that controls it.
//!
//! A Terminal Session tells its subscription when it has something new: a screen, an exit, a
//! client effect, hidden input or an accessibility model. The subscription's notifier wakes the
//! server's publisher ([`PublishSignal`]), which takes the `PaneService` lock and lets each Pane
//! drain its session and publish what changed through its [`Publication`]. A screen is captured
//! as an `Arc` under that lock and encoded later on the connection's I/O thread by
//! [`ScreenPresentation`]. Nothing here polls: the publisher waits for a notification or for the
//! next deadline the service reports.
#![cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "the push cutover wires publication into the PaneService"
    )
)]
use super::*;
use crate::terminal::session::SubscriptionNotifier;
use spaceterm_server::connection::outbox::{
    FrameTicket, Outbox, OutboxError, Presentation, Presented,
};
use spaceterm_server::protocol::{Channel, Fatal, PaneEvent, ScreenUpdate};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Instant;

/// Wakes the server's publisher. A notification before the wait is never lost, and several
/// notifications before one wait wake it once.
#[derive(Clone, Default)]
pub(crate) struct PublishSignal {
    shared: Arc<(Mutex<bool>, Condvar)>,
}

impl PublishSignal {
    pub(crate) fn notify(&self) {
        let (pending, ready) = &*self.shared;
        *pending.lock().unwrap_or_else(|e| e.into_inner()) = true;
        ready.notify_all();
    }

    /// A notifier for a Terminal Session subscription.
    pub(crate) fn notifier(&self) -> SubscriptionNotifier {
        let signal = self.clone();
        Arc::new(move || signal.notify())
    }

    /// Returns on the first notification since the previous wait, or at `deadline`.
    pub(crate) fn wait(&self, deadline: Option<Instant>) {
        let (pending, ready) = &*self.shared;
        let mut pending = pending.lock().unwrap_or_else(|e| e.into_inner());
        while !*pending {
            pending = match deadline {
                None => ready.wait(pending).unwrap_or_else(|e| e.into_inner()),
                Some(deadline) => {
                    let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                        return;
                    };
                    ready
                        .wait_timeout(pending, remaining)
                        .unwrap_or_else(|e| e.into_inner())
                        .0
                }
            };
        }
        *pending = false;
    }
}

/// Runs `publish` after each notification and at each deadline it returns, until `running`
/// turns false. Call [`PublishSignal::notify`] after clearing `running` to stop it at once.
pub(crate) fn run_publisher(
    signal: &PublishSignal,
    running: &AtomicBool,
    mut publish: impl FnMut(Instant) -> Option<Instant>,
) {
    while running.load(Ordering::Acquire) {
        let deadline = publish(Instant::now());
        signal.wait(deadline);
    }
}

/// A Pane screen captured under the `PaneService` lock.
#[derive(Clone)]
pub(super) struct Captured {
    pub(super) screen: Arc<ScreenSnapshot>,
    /// The wire screen generation, which fences pointer and wheel coordinates.
    pub(super) generation: u64,
    pub(super) accessibility_revision: u64,
}

/// One channel's screen encoder. It keeps the style and link tables and the rows of the frame
/// the client acknowledged.
pub(super) trait ScreenEncoding: Send {
    /// Encodes `screen` against `ticket.base`, or as a keyframe that resets the tables when
    /// `base` is `None`. Returns `None` when `screen` equals the base; the baseline then stays.
    fn encode(&mut self, screen: &Captured, ticket: FrameTicket) -> Option<ScreenUpdate>;
}

type SharedEncoder = Arc<Mutex<Box<dyn ScreenEncoding>>>;

/// A captured screen waiting for credit. It owns its snapshot and encodes it on the I/O thread.
pub(super) struct ScreenPresentation {
    captured: Captured,
    encoder: SharedEncoder,
}

impl Presentation for ScreenPresentation {
    fn encode(self: Box<Self>, ticket: FrameTicket) -> Option<Presented> {
        let update = self
            .encoder
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .encode(&self.captured, ticket)?;
        Some(Presented {
            input: self.captured.screen.input_serial,
            update,
        })
    }
}

/// The channel that presents one Pane, if a claim bound one.
#[derive(Default)]
pub(super) struct Publication {
    target: Option<Target>,
}

struct Target {
    outbox: Outbox,
    channel: Channel,
    visible: bool,
    encoder: SharedEncoder,
    /// The wire screen generation last handed to the outbox while visible.
    presented: Option<u64>,
    hidden_input: Option<bool>,
}

impl Publication {
    /// Opens `channel` on `outbox` for an attached claim. The channel starts visible and owes a
    /// keyframe. A previous channel is forgotten; end it first if its client must learn why.
    pub(super) fn bind(
        &mut self,
        outbox: Outbox,
        channel: Channel,
        encoder: Box<dyn ScreenEncoding>,
    ) -> Result<(), OutboxError> {
        self.target = None;
        outbox.open(channel)?;
        self.target = Some(Target {
            outbox,
            channel,
            visible: true,
            encoder: Arc::new(Mutex::new(encoder)),
            presented: None,
            hidden_input: None,
        });
        Ok(())
    }

    /// Forgets the channel after the client released or closed it. The connection already
    /// closed it on its outbox.
    pub(super) fn unbind(&mut self) {
        self.target = None;
    }

    pub(super) fn is_bound(&self) -> bool {
        self.target.is_some()
    }

    /// Applies a client `View`. The connection already applied it to the outbox, which drops
    /// pending state while hidden. Revealing presents `current` again; the encoder compares it
    /// with the acknowledged frame, so an unchanged screen sends nothing.
    pub(super) fn view(&mut self, visible: bool, current: Option<&Captured>) {
        let Some(target) = &mut self.target else {
            return;
        };
        target.visible = visible;
        if visible {
            target.presented = None;
            if let Some(current) = current {
                self.screen(current);
            }
        }
    }

    /// Presents `captured` if the channel is visible and has not been handed this screen.
    /// Returns whether it was presented.
    pub(super) fn screen(&mut self, captured: &Captured) -> bool {
        let Some(target) = &mut self.target else {
            return false;
        };
        if !target.visible || target.presented == Some(captured.generation) {
            return false;
        }
        target.presented = Some(captured.generation);
        let presentation = Box::new(ScreenPresentation {
            captured: captured.clone(),
            encoder: Arc::clone(&target.encoder),
        });
        let result = target.outbox.present(target.channel, presentation);
        self.deliver(result)
    }

    /// Sends `HiddenInput` when the state differs from what the channel was last told.
    pub(super) fn hidden_input(&mut self, active: bool) {
        let Some(target) = &mut self.target else {
            return;
        };
        if target.hidden_input != Some(active) {
            target.hidden_input = Some(active);
            self.event(PaneEvent::HiddenInput(active));
        }
    }

    /// Queues a Pane event that leaves the channel open.
    pub(super) fn event(&mut self, event: PaneEvent) {
        let Some(target) = &self.target else {
            return;
        };
        let result = target.outbox.pane(target.channel, event);
        self.deliver(result);
    }

    /// Ends the channel with `Ended`, `SessionEnded`, `Failed` or `Takeover`, after presenting
    /// `last` so the client keeps the final screen.
    pub(super) fn end(&mut self, last: Option<&Captured>, event: PaneEvent) {
        if let Some(last) = last {
            self.screen(last);
        }
        let Some(target) = &self.target else {
            return;
        };
        let result = target.outbox.end(target.channel, event);
        self.deliver(result);
        self.target = None;
    }

    /// Keeps the channel after a delivery, or forgets it when its connection can take no more.
    /// A connection that broke a bound or misused its outbox ends with a goodbye.
    fn deliver(&mut self, result: Result<(), OutboxError>) -> bool {
        let Some(target) = &self.target else {
            return false;
        };
        match result {
            Ok(()) => true,
            // A message this server cannot encode is dropped; the channel continues.
            Err(OutboxError::Encode(_)) => false,
            Err(OutboxError::Closed) => {
                self.target = None;
                false
            }
            Err(error) => {
                let _ = target.outbox.goodbye(Fatal::from(error));
                self.target = None;
                false
            }
        }
    }
}

/// What a Pane's publication found about its Terminal Session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Lifecycle {
    Live,
    /// The shell exited. The channel received `Ended`.
    Ended,
    /// The Terminal Session failed. The channel received `Failed`.
    Failed,
}

impl OwnedPane {
    /// The current screen as a [`Captured`], for a `Presentation` that outlives the lock.
    pub(super) fn capture(&self) -> Option<Captured> {
        self.screen.as_ref().map(|screen| Captured {
            screen: Arc::clone(screen),
            generation: self.screen_generation,
            accessibility_revision: self.accessibility.revision(),
        })
    }

    /// Drains the Terminal Session and pushes what changed: the screen, client effects under
    /// the current focus generation, hidden input and the accessibility revision. An exit or
    /// failure presents the final screen, then ends the channel.
    pub(super) fn publish(&mut self) -> Lifecycle {
        self.drain();
        if self.publication.is_bound() && self.focus_generation != 0 {
            while let Some(request) = self.effects.next(self.focus_generation) {
                self.publication.event(PaneEvent::Effect(request));
            }
        }
        self.publication
            .hidden_input(self.subscription.hidden_input());
        let captured = self.capture();
        if self.failed {
            self.publication.end(captured.as_ref(), PaneEvent::Failed);
            return Lifecycle::Failed;
        }
        if self.ended {
            self.publication.end(captured.as_ref(), PaneEvent::Ended);
            return Lifecycle::Ended;
        }
        let carried = captured
            .as_ref()
            .is_some_and(|captured| self.publication.screen(captured));
        // A presented screen carries the revision. Otherwise the change travels as an event.
        if let Some(revision) = self.accessibility.take_changed_revision()
            && !carried
        {
            self.publication
                .event(PaneEvent::Accessibility { revision });
        }
        Lifecycle::Live
    }

    /// Applies a client `View`: the Terminal Session publishes only while visible, at the
    /// declared frame rate, and a revealed channel receives the current screen again.
    pub(super) fn view(&mut self, visible: bool, max_hz: u8) {
        if self.presentable != visible {
            self.presentable = visible;
            self.runtime.set_presentable(visible);
            if !visible {
                self.runtime.end_server_accessibility_demand();
            }
        }
        self.runtime.set_frame_rate(max_hz);
        let captured = self.capture();
        self.publication.view(visible, captured.as_ref());
    }
}

#[cfg(test)]
#[path = "publish_tests.rs"]
mod tests;
