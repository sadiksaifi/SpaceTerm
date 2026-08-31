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
}

impl LiveConnectionAuthority {
    pub(crate) fn new(socket: RegisteredRuntimeSocket) -> Arc<Self> {
        Arc::new(Self {
            state: AtomicU8::new(LiveConnectionState::Ready as u8),
            generation: AtomicU64::new(1),
            cancellation: Mutex::new(SshCancellationToken::default()),
            socket,
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
    }

    pub(crate) fn state(&self) -> LiveConnectionState {
        LiveConnectionState::from_raw(self.state.load(Ordering::Acquire))
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
