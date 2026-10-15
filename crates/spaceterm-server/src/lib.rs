//! Private local SpaceTerm Protocol and explicit Development-server setup.
#[cfg(feature = "client")]
mod client;
mod host;
mod protocol;
mod storage;

#[cfg(feature = "client")]
pub use client::LocalServer;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuthenticatedStatus {
    pub protocol_major: u16,
}

/// Bounded classifications. Native errors and peer-supplied values never cross this boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ServerError {
    #[error("This device has not been paired with the local server.")]
    NotPaired,
    #[error("The local server is not running.")]
    NotRunning,
    #[error("Native credential storage is locked or unavailable. Unlock it and try again.")]
    CredentialUnavailable,
    #[error("Local server storage is unavailable.")]
    StorageUnavailable,
    #[error("Local server storage has unsafe ownership or permissions.")]
    UnsafeStorage,
    #[error("The local server could not authenticate this connection.")]
    AuthenticationRefused,
    #[error("The client and server have no compatible protocol version.")]
    IncompatibleProtocol,
    #[error("The local server connection did not complete.")]
    Protocol,
    #[error("The Development server could not start. Rebuild with mise run development.")]
    StartupFailed,
}

/// Runs only an explicitly requested Development server. No service registration or listener
/// is installed by library construction or desktop launch.
pub fn run_development() -> Result<(), ServerError> {
    let paths = host::Paths::for_identity("spaceterm-development")?;
    host::serve(&paths)
}
