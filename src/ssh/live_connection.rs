use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use thiserror::Error;

use super::cancellation::SshCancellationToken;
use crate::platform::app_paths::RegisteredRuntimeSocket;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum LiveConnectionState {
    Ready = 0,
    ShuttingDown = 1,
    Failed = 2,
    Closed = 3,
}

impl LiveConnectionState {
    fn from_raw(value: u8) -> Self {
        match value {
            0 => Self::Ready,
            1 => Self::ShuttingDown,
            2 => Self::Failed,
            _ => Self::Closed,
        }
    }
}

pub(crate) struct LiveConnectionAuthority {
    state: AtomicU8,
    generation: AtomicU64,
    cancellation: Mutex<SshCancellationToken>,
    socket: RegisteredRuntimeSocket,
    lifecycle: ControlConnectionLifecycleAuthority,
}

impl LiveConnectionAuthority {
    pub(crate) fn new(socket: RegisteredRuntimeSocket) -> Arc<Self> {
        Arc::new(Self {
            state: AtomicU8::new(LiveConnectionState::Ready as u8),
            generation: AtomicU64::new(1),
            cancellation: Mutex::new(SshCancellationToken::default()),
            socket,
            lifecycle: ControlConnectionLifecycleAuthority::default(),
        })
    }

    pub(crate) fn capability(self: &Arc<Self>) -> LiveConnectionCapability {
        LiveConnectionCapability {
            authority: Arc::downgrade(self),
            generation: self.generation.load(Ordering::Acquire),
            cancellation: self
                .cancellation
                .lock()
                .map_or_else(|_| SshCancellationToken::cancelled(), |token| token.clone()),
        }
    }

    pub(crate) fn transition(&self, state: LiveConnectionState) {
        if let Ok(mut cancellation) = self.cancellation.lock() {
            cancellation.cancel();
            if state == LiveConnectionState::Ready {
                *cancellation = SshCancellationToken::default();
            }
        }
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.state.store(state as u8, Ordering::Release);
        match state {
            LiveConnectionState::Failed => self
                .lifecycle
                .publish(ControlConnectionTerminalState::Failed),
            LiveConnectionState::Closed => self
                .lifecycle
                .publish(ControlConnectionTerminalState::Closed),
            LiveConnectionState::Ready | LiveConnectionState::ShuttingDown => {}
        }
    }

    pub(crate) fn state(&self) -> LiveConnectionState {
        LiveConnectionState::from_raw(self.state.load(Ordering::Acquire))
    }

    pub(crate) fn observe_lifecycle(&self) -> ControlConnectionLifecycleObserver {
        self.lifecycle.observe()
    }
}

impl Drop for LiveConnectionAuthority {
    fn drop(&mut self) {
        self.lifecycle
            .publish(ControlConnectionTerminalState::Closed);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ControlConnectionTerminalState {
    Failed = 1,
    Closed = 2,
}

impl ControlConnectionTerminalState {
    fn from_raw(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Failed),
            2 => Some(Self::Closed),
            _ => None,
        }
    }
}

#[derive(Default)]
struct ControlConnectionLifecycleAuthority {
    terminal: AtomicU8,
    observers: Mutex<Vec<async_channel::Sender<ControlConnectionTerminalState>>>,
}

impl ControlConnectionLifecycleAuthority {
    fn observe(&self) -> ControlConnectionLifecycleObserver {
        let (sender, receiver) = async_channel::bounded(1);
        let terminal = self.terminal.load(Ordering::Acquire);
        if let Some(terminal) = ControlConnectionTerminalState::from_raw(terminal) {
            let _ = sender.try_send(terminal);
        } else if let Ok(mut observers) = self.observers.lock() {
            let terminal = self.terminal.load(Ordering::Acquire);
            if let Some(terminal) = ControlConnectionTerminalState::from_raw(terminal) {
                let _ = sender.try_send(terminal);
            } else {
                observers.push(sender);
            }
        } else {
            let _ = sender.try_send(ControlConnectionTerminalState::Failed);
        }
        ControlConnectionLifecycleObserver { receiver }
    }

    fn publish(&self, terminal: ControlConnectionTerminalState) {
        if self
            .terminal
            .compare_exchange(0, terminal as u8, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        if let Ok(mut observers) = self.observers.lock() {
            for observer in observers.drain(..) {
                let _ = observer.try_send(terminal);
            }
        }
    }
}

pub(crate) struct ControlConnectionLifecycleObserver {
    receiver: async_channel::Receiver<ControlConnectionTerminalState>,
}

pub(crate) type ControlConnectionObserver = ControlConnectionLifecycleObserver;

impl ControlConnectionLifecycleObserver {
    pub(crate) async fn terminal(&self) -> ControlConnectionTerminalState {
        self.receiver
            .recv()
            .await
            .unwrap_or(ControlConnectionTerminalState::Closed)
    }

    #[cfg(test)]
    pub(crate) fn closed() -> Self {
        let authority = ControlConnectionLifecycleAuthority::default();
        authority.publish(ControlConnectionTerminalState::Closed);
        authority.observe()
    }
}

#[derive(Clone)]
pub(crate) struct LiveConnectionCapability {
    authority: Weak<LiveConnectionAuthority>,
    generation: u64,
    cancellation: SshCancellationToken,
}

impl LiveConnectionCapability {
    pub(crate) fn authorize(&self) -> Result<(), LiveConnectionError> {
        let authority = self
            .authority
            .upgrade()
            .ok_or(LiveConnectionError::Unavailable)?;
        if authority.state() != LiveConnectionState::Ready
            || authority.generation.load(Ordering::Acquire) != self.generation
            || self.cancellation.is_cancelled()
        {
            return Err(LiveConnectionError::Unavailable);
        }
        authority
            .socket
            .verify()
            .map_err(|_| LiveConnectionError::SocketReplaced)
    }

    pub(crate) fn cancellation(&self) -> SshCancellationToken {
        self.cancellation.clone()
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(crate) enum LiveConnectionError {
    #[error("the SSH control connection is no longer available")]
    Unavailable,
    #[error("the SSH control socket identity changed")]
    SocketReplaced,
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::task::{Context, Poll, Wake, Waker};

    use super::*;

    fn block_on<T>(future: impl Future<Output = T>) -> T {
        struct ThreadWake(std::thread::Thread);
        impl Wake for ThreadWake {
            fn wake(self: Arc<Self>) {
                self.0.unpark();
            }
        }
        let waker = Waker::from(Arc::new(ThreadWake(std::thread::current())));
        let mut context = Context::from_waker(&waker);
        let mut future = Box::pin(future);
        loop {
            match Pin::new(&mut future).poll(&mut context) {
                Poll::Ready(output) => return output,
                Poll::Pending => std::thread::park(),
            }
        }
    }

    #[test]
    fn lifecycle_observer_should_publish_the_first_terminal_outcome_exactly_once() {
        let authority = ControlConnectionLifecycleAuthority::default();
        let observer = authority.observe();

        authority.publish(ControlConnectionTerminalState::Failed);
        authority.publish(ControlConnectionTerminalState::Closed);

        assert_eq!(
            block_on(observer.terminal()),
            ControlConnectionTerminalState::Failed
        );
        assert!(observer.receiver.try_recv().is_err());
    }

    #[test]
    fn lifecycle_observer_should_report_an_outcome_published_before_observation() {
        let authority = ControlConnectionLifecycleAuthority::default();
        authority.publish(ControlConnectionTerminalState::Closed);

        assert_eq!(
            block_on(authority.observe().terminal()),
            ControlConnectionTerminalState::Closed
        );
    }

    #[test]
    fn lifecycle_observer_should_not_treat_nonterminal_transitions_as_events() {
        let authority = ControlConnectionLifecycleAuthority::default();
        let observer = authority.observe();

        assert!(observer.receiver.try_recv().is_err());
        authority.publish(ControlConnectionTerminalState::Closed);
        assert_eq!(
            block_on(observer.terminal()),
            ControlConnectionTerminalState::Closed
        );
    }
}
