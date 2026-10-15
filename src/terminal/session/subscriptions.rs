//! Bounded presentation delivery, independent of worker and PTY ownership.
use super::*;

pub struct TerminalSubscription {
    pub events: async_channel::Receiver<TerminalSessionEvent>,
    pub accessibility: async_channel::Receiver<Arc<TerminalAccessibilityModel>>,
    pub clipboard: async_channel::Receiver<ClipboardRequest>,
}

#[derive(Clone)]
pub(super) struct TerminalEventPublisher(Arc<Mutex<EventState>>);

struct EventState {
    events: async_channel::Sender<TerminalSessionEvent>,
    screen: Option<Arc<ScreenSnapshot>>,
    finished: Option<Result<TerminalSessionExit, TerminalSessionFailure>>,
    stopped: bool,
}

impl From<async_channel::Sender<TerminalSessionEvent>> for TerminalEventPublisher {
    fn from(events: async_channel::Sender<TerminalSessionEvent>) -> Self {
        Self(Arc::new(Mutex::new(EventState {
            events,
            screen: None,
            finished: None,
            stopped: false,
        })))
    }
}

impl TerminalEventPublisher {
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
        if state.stopped {
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
        state.events.close();
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
            TerminalSessionEvent::Exited(exit) => self.finished = Some(Ok(exit.clone())),
            TerminalSessionEvent::Failed(failure) => self.finished = Some(Err(failure.clone())),
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
