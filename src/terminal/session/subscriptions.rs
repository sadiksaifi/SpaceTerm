//! Bounded presentation delivery, independent of worker and PTY ownership.
use super::*;
use std::sync::{Condvar, Mutex as PublicationMutex};
use std::time::Instant;

/// Counts screen, exit and failure publications so a waiter can observe the next one without
/// consuming any event. Closing the publisher wakes every waiter.
#[derive(Clone, Default)]
pub struct Publications(Arc<(PublicationMutex<PublicationCount>, Condvar)>);

#[derive(Default)]
struct PublicationCount {
    count: u64,
    closed: bool,
}

impl Publications {
    /// The number of publications so far.
    pub fn current(&self) -> u64 {
        self.0.0.lock().unwrap_or_else(|e| e.into_inner()).count
    }

    /// Blocks until a publication after `seen`, closure, or `deadline`. Returns whether a
    /// publication after `seen` happened.
    pub fn wait_after(&self, seen: u64, deadline: Instant) -> bool {
        let (count, ready) = &*self.0;
        let mut state = count.lock().unwrap_or_else(|e| e.into_inner());
        while state.count == seen && !state.closed {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                break;
            };
            state = ready
                .wait_timeout(state, remaining)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        state.count != seen
    }

    fn publish(&self) {
        let (count, ready) = &*self.0;
        let mut state = count.lock().unwrap_or_else(|e| e.into_inner());
        state.count = state.count.wrapping_add(1);
        ready.notify_all();
    }

    fn close(&self) {
        let (count, ready) = &*self.0;
        count.lock().unwrap_or_else(|e| e.into_inner()).closed = true;
        ready.notify_all();
    }
}

pub struct TerminalSubscription {
    pub events: async_channel::Receiver<TerminalSessionEvent>,
    pub accessibility: async_channel::Receiver<Arc<TerminalAccessibilityModel>>,
    pub clipboard: async_channel::Receiver<ClipboardRequest>,
    pub(super) state: TerminalEventPublisher,
}

impl TerminalSubscription {
    pub(crate) fn take_client_effects(&self) -> Vec<ClientEffectRequest> {
        self.state
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .client_effects
            .drain(..)
            .collect()
    }

    /// The retained PTY hidden-input state. Read it after receiving events to reconcile
    /// coalesced change notifications. A stopped runtime always returns false.
    pub fn hidden_input(&self) -> bool {
        self.state.hidden_input()
    }

    /// The publications of this subscription's runtime, observable without consuming events.
    pub fn publications(&self) -> Publications {
        self.state
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .publications
            .clone()
    }
}

#[derive(Clone)]
pub(super) struct TerminalEventPublisher(Arc<Mutex<EventState>>);

struct EventState {
    events: async_channel::Sender<TerminalSessionEvent>,
    screen: Option<Arc<ScreenSnapshot>>,
    finished: Option<Result<TerminalSessionExit, TerminalSessionFailure>>,
    stopped: bool,
    hidden_input: bool,
    client_effects: std::collections::VecDeque<ClientEffectRequest>,
    publications: Publications,
}

impl From<async_channel::Sender<TerminalSessionEvent>> for TerminalEventPublisher {
    fn from(events: async_channel::Sender<TerminalSessionEvent>) -> Self {
        Self(Arc::new(Mutex::new(EventState {
            events,
            screen: None,
            finished: None,
            stopped: false,
            hidden_input: false,
            client_effects: std::collections::VecDeque::new(),
            publications: Publications::default(),
        })))
    }
}

impl TerminalEventPublisher {
    pub(super) fn client_effect(&self, request: ClientEffectRequest) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        state
            .client_effects
            .retain(|request| request.grant.current());
        if state.client_effects.len() < 8 {
            state.client_effects.push_back(request);
        }
    }

    pub(super) fn subscribe(
        &self,
    ) -> (
        async_channel::Receiver<TerminalSessionEvent>,
        async_channel::Sender<TerminalSessionEvent>,
    ) {
        let (events, receiver) = async_channel::bounded(2);
        let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        state.events.close();
        state.events = events.clone();
        if state.stopped || state.finished.is_some() {
            if let Some(screen) = &state.screen {
                let _ = events.try_send(TerminalSessionEvent::Screen(Arc::clone(screen)));
            }
            if let Some(finished) = &state.finished {
                let _ = events.try_send(match finished {
                    Ok(exit) => TerminalSessionEvent::Exited(exit.clone()),
                    Err(failure) => TerminalSessionEvent::Failed(failure.clone()),
                });
            }
            events.close();
        }
        (receiver, events)
    }

    pub(super) fn is_closed(&self) -> bool {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .events
            .is_closed()
    }

    pub(super) fn close(&self) {
        let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        state.stopped = true;
        state.hidden_input = false;
        state.events.close();
        state.publications.close();
    }

    pub(super) fn hidden_input(&self) -> bool {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .hidden_input
    }

    pub(super) fn try_send(
        &self,
        event: TerminalSessionEvent,
    ) -> Result<(), async_channel::TrySendError<TerminalSessionEvent>> {
        let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        state.retain(&event);
        let counted = counted(&event);
        let sent = state.events.try_send(event);
        if counted {
            state.publications.publish();
        }
        sent
    }

    pub(super) fn force_send(
        &self,
        event: TerminalSessionEvent,
    ) -> Result<Option<TerminalSessionEvent>, async_channel::SendError<TerminalSessionEvent>> {
        let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        state.retain(&event);
        let counted = counted(&event);
        let sent = state.events.force_send(event);
        if counted {
            state.publications.publish();
        }
        sent
    }

    pub(super) fn repeat_screen(&self) {
        let state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(screen) = &state.screen {
            let _ = state
                .events
                .force_send(TerminalSessionEvent::Screen(Arc::clone(screen)));
            state.publications.publish();
        }
    }
}

/// Screens and session ends are the publications a waiter observes. Counting happens after the
/// send, so a woken waiter finds the event already queued for the subscriber.
fn counted(event: &TerminalSessionEvent) -> bool {
    matches!(
        event,
        TerminalSessionEvent::Screen(_)
            | TerminalSessionEvent::Exited(_)
            | TerminalSessionEvent::Failed(_)
    )
}

impl EventState {
    fn retain(&mut self, event: &TerminalSessionEvent) {
        match event {
            TerminalSessionEvent::Screen(screen) => self.screen = Some(Arc::clone(screen)),
            TerminalSessionEvent::Exited(exit) => {
                self.hidden_input = false;
                self.finished = Some(Ok(exit.clone()));
            }
            TerminalSessionEvent::Failed(failure) => {
                self.hidden_input = false;
                self.finished = Some(Err(failure.clone()));
            }
            TerminalSessionEvent::HiddenInputChanged(active) => self.hidden_input = *active,
            _ => {}
        }
    }
}

pub(super) struct WorkerEventLifetime(pub(super) TerminalEventPublisher);

impl Drop for WorkerEventLifetime {
    fn drop(&mut self) {
        self.0.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_waiter_observes_a_publication_without_consuming_it_and_closing_wakes_it() {
        let (events, receiver) = async_channel::bounded(2);
        let publisher = TerminalEventPublisher::from(events);
        let publications = publisher.0.lock().unwrap().publications.clone();
        let seen = publications.current();
        let waiter = {
            let publications = publications.clone();
            std::thread::spawn(move || {
                publications.wait_after(seen, Instant::now() + Duration::from_secs(5))
            })
        };
        let screen = ScreenSnapshot::empty(crate::local_path::LocalPathSemantics::Posix);
        publisher
            .try_send(TerminalSessionEvent::Screen(Arc::clone(&screen)))
            .unwrap();
        assert!(waiter.join().unwrap());
        assert!(
            matches!(receiver.try_recv(), Ok(TerminalSessionEvent::Screen(actual)) if Arc::ptr_eq(&actual, &screen))
        );

        let seen = publications.current();
        let started = Instant::now();
        assert!(!publications.wait_after(seen, started + Duration::from_millis(20)));
        assert!(started.elapsed() >= Duration::from_millis(20));

        publisher.close();
        let started = Instant::now();
        assert!(!publications.wait_after(seen, started + Duration::from_secs(5)));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn replacement_replays_completion_before_worker_teardown_closes_the_publisher() {
        let (events, _receiver) = async_channel::bounded(2);
        let publisher = TerminalEventPublisher::from(events);
        let screen = ScreenSnapshot::empty(crate::local_path::LocalPathSemantics::Posix);
        publisher
            .force_send(TerminalSessionEvent::Screen(Arc::clone(&screen)))
            .unwrap();
        publisher
            .try_send(TerminalSessionEvent::Exited(TerminalSessionExit::Success))
            .unwrap();

        // A subscriber can replace its channel after final publication but before worker teardown.
        let (replacement, _) = publisher.subscribe();
        publisher.close();

        assert!(
            matches!(replacement.try_recv(), Ok(TerminalSessionEvent::Screen(actual)) if Arc::ptr_eq(&actual, &screen))
        );
        assert!(matches!(
            replacement.try_recv(),
            Ok(TerminalSessionEvent::Exited(TerminalSessionExit::Success))
        ));
        assert!(replacement.try_recv().is_err());
    }
}
