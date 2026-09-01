use crate::terminal::RemoteChannelRevalidationError;

/// A content-free reason that a requested Remote child Terminal could not be launched.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RemoteChildLaunchUnavailable {
    ConnectionUnavailable,
    DirectoryUnavailable,
    IdentityChanged,
    Cancelled,
    Stale,
}

impl From<RemoteChannelRevalidationError> for RemoteChildLaunchUnavailable {
    fn from(error: RemoteChannelRevalidationError) -> Self {
        match error {
            RemoteChannelRevalidationError::ConnectionUnavailable => Self::ConnectionUnavailable,
            RemoteChannelRevalidationError::DirectoryUnavailable => Self::DirectoryUnavailable,
            RemoteChannelRevalidationError::IdentityChanged => Self::IdentityChanged,
        }
    }
}
