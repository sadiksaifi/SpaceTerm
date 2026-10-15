//! Private local SpaceTerm Protocol and explicit local server setup.
pub mod attachments;
#[cfg(feature = "client")]
mod client;
pub mod client_view;
pub mod hierarchy;
mod host;
pub mod organization;
pub mod pane;
mod protocol;
pub mod repository;
mod storage;

#[cfg(feature = "client")]
pub use client::LocalServer;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuthenticatedStatus {
    pub protocol_major: u16,
    pub development_pane: bool,
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
    #[error("The local server could not start. Reinstall SpaceTerm and try again.")]
    StartupFailed,
    #[error("This Pane is controlled by another device.")]
    Controlled,
    #[error("This Terminal Session is no longer available.")]
    SessionUnavailable,
    #[error("This connection no longer controls the Terminal Session.")]
    StaleControl,
    #[error("This operation is unavailable for this Development Pane.")]
    Unsupported,
    #[error("Directory unavailable.")]
    DirectoryUnavailable,
    #[error("Terminal cleanup could not be confirmed.")]
    CleanupUnresolved,
    #[error(
        "The server is waiting for shared edit acknowledgements. Reconnect the other clients to continue."
    )]
    Capacity,
}

/// Runs an explicitly requested local server. No service registration or listener
/// is installed by library construction or desktop launch.
pub fn run_local(identity: &str) -> Result<(), ServerError> {
    host::install_shutdown_signals()?;
    let paths = host::Paths::for_identity(identity)?;
    host::serve(&paths)
}

pub fn run_local_with_panes(
    identity: &str,
    panes: impl pane::PaneService + 'static,
) -> Result<(), ServerError> {
    if identity != "spaceterm-development" {
        return Err(ServerError::Unsupported);
    }
    host::install_shutdown_signals()?;
    host::serve_with_panes(&host::Paths::for_identity(identity)?, Some(Box::new(panes)))
}
