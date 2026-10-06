use crate::terminal::TerminalSessionChannelRevalidationError;

/// A content-free reason that a requested Remote child Terminal Session could not be launched.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RemoteChildLaunchUnavailable {
    ConnectionUnavailable,
    DirectoryUnavailable,
    IdentityChanged,
    Cancelled,
    Stale,
}

impl From<TerminalSessionChannelRevalidationError> for RemoteChildLaunchUnavailable {
    fn from(error: TerminalSessionChannelRevalidationError) -> Self {
        match error {
            TerminalSessionChannelRevalidationError::ConnectionUnavailable => {
                Self::ConnectionUnavailable
            }
            TerminalSessionChannelRevalidationError::DirectoryUnavailable => {
                Self::DirectoryUnavailable
            }
            TerminalSessionChannelRevalidationError::IdentityChanged => Self::IdentityChanged,
        }
    }
}
