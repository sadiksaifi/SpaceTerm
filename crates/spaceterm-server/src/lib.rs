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
pub mod service;
mod storage;
pub mod worktree;

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
    #[error("The local server is not set up.")]
    NotSetUp,
    #[error("The local server service is unavailable. Try again after signing in.")]
    ServiceUnavailable,
    #[error("Install the local server service first.")]
    NotInstalled,
    #[error("The local server was stopped. Use Start Server to start it again.")]
    Stopped,
    #[error("This server package could not be verified for this application identity.")]
    RuntimeUnavailable,
    #[error("Administrator authorization is required to enable linger for this account.")]
    AuthorizationRequired,
    #[error("The local server is not running.")]
    NotRunning,
    #[error("Local server storage is unavailable.")]
    StorageUnavailable,
    #[error(
        "This server's saved state is from an incompatible SpaceTerm build. SpaceTerm left it unchanged."
    )]
    IncompatibleState,
    #[error("Local server storage has unsafe ownership or permissions.")]
    UnsafeStorage,
    #[error(
        "The connection did not pass the account and server identity checks, so SpaceTerm did not connect."
    )]
    AuthenticationRefused,
    #[error("The client and server have no compatible protocol version.")]
    IncompatibleProtocol,
    #[error("The local server connection did not complete.")]
    Protocol,
    #[error("The local server could not start. Reinstall SpaceTerm and try again.")]
    StartupFailed,
    #[error("Another client controls this Pane.")]
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
    host::install_shutdown_signals()?;
    host::serve_with_panes(&host::Paths::for_identity(identity)?, Some(Box::new(panes)))
}
