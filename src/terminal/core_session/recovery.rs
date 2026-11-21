//! Classifies desktop connection errors for portable terminal recovery.
use super::{CoreError, DirectError};
use spaceterm_terminal_client::recovery::{FailureKind, Failures};

pub(super) type Recovery = spaceterm_terminal_client::recovery::Recovery<DesktopFailures>;

pub(super) struct DesktopFailures;

impl Failures for DesktopFailures {
    type Error = CoreError;

    const PROTOCOL: CoreError = CoreError::Protocol;

    fn kind(error: CoreError) -> FailureKind {
        match error {
            CoreError::Protocol => FailureKind::Protocol,
            CoreError::Direct(DirectError::Transport) => FailureKind::PairedTransport,
            CoreError::NotRunning => FailureKind::LocalTransport,
            _ => FailureKind::Other,
        }
    }
}
