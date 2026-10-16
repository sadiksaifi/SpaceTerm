//! Bounded presentation delivery, independent of worker and PTY ownership.
use super::*;

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
        state.events.try_send(event)
    }

    pub(super) fn force_send(
        &self,
        event: TerminalSessionEvent,
    ) -> Result<Option<TerminalSessionEvent>, async_channel::SendError<TerminalSessionEvent>> {
        let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        state.retain(&event);
        state.events.force_send(event)
    }

    pub(super) fn repeat_screen(&self) {
        let state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(screen) = &state.screen {
            let _ = state
                .events
                .force_send(TerminalSessionEvent::Screen(Arc::clone(screen)));
        }
    }
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
