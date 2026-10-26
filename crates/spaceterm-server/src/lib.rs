//! SpaceTerm Protocol admission, direct-network access and account server setup.
pub mod attachments;
#[cfg(feature = "client")]
mod client;
pub mod client_view;
#[cfg(feature = "client")]
mod computer_name;
pub mod direct;
pub mod hierarchy;
mod host;
pub mod organization;
pub mod pane;
mod protocol;
pub mod provisioning;
pub mod repository;
pub mod service;
#[cfg(feature = "client")]
mod startup;
mod storage;
mod transport;
pub mod workspace_order;
pub mod worktree;

#[cfg(feature = "client")]
pub use client::{ClientScope, ServerConnection};
#[cfg(feature = "client")]
pub use startup::{LocalStartup, StartupOutcome};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuthenticatedStatus {
    pub protocol_major: u16,
    pub development_pane: bool,
}

/// Bounded classifications. Native errors and peer-supplied values never cross this boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ServerError {
    #[error("{0}")]
    Direct(crate::direct::DirectError),
    #[error("The server is not set up.")]
    NotSetUp,
    #[error("Server service unavailable. Sign in and try again.")]
    ServiceUnavailable,
    #[error("Install the server service first.")]
    NotInstalled,
    #[error("The local server was stopped. Use Start Server to start it again.")]
    Stopped,
    #[error("The server changed. Review and confirm again.")]
    Changed,
    #[error(
        "SpaceTerm couldn't confirm whether the server update finished. Check the update in Server Settings."
    )]
    ActivationUnresolved,
    #[error("SpaceTerm can't safely restore the previous server version.")]
    ReturnUnavailable,
    #[error("Couldn't verify this server package.")]
    RuntimeUnavailable,
    /// This build includes no server package to verify. Distinct from a package that failed
    /// verification.
    #[error("This SpaceTerm build doesn't include a server package.")]
    CandidateMissing,
    #[error("Administrator authorization required.")]
    AuthorizationRequired,
    #[error("The server is not running.")]
    NotRunning,
    #[error("Server storage is unavailable.")]
    StorageUnavailable,
    #[error("This server's saved state is from an incompatible SpaceTerm.")]
    IncompatibleState,
    #[error("Server storage has unsafe permissions.")]
    UnsafeStorage,
    #[error("SpaceTerm couldn't verify the server or account for this connection.")]
    AuthenticationRefused,
    #[error("SpaceTerm and this server are incompatible.")]
    IncompatibleProtocol,
    #[error("This server needs an update.")]
    MissingRequiredCapability,
    #[error(
        "SpaceTerm was updated or moved after it opened. Quit and reopen SpaceTerm to continue."
    )]
    CandidateChanged,
    #[error("The server connection did not complete.")]
    Protocol,
    #[error("Reinstall SpaceTerm and try again.")]
    StartupFailed,
    #[error("Another client controls this Pane.")]
    Controlled,
    #[error("This Terminal Session is unavailable.")]
    SessionUnavailable,
    #[error("This connection no longer controls the Terminal Session.")]
    StaleControl,
    #[error("This operation isn't supported.")]
    Unsupported,
    #[error("Directory unavailable.")]
    DirectoryUnavailable,
    #[error("Couldn't confirm the Terminal Sessions ended.")]
    CleanupUnresolved,
    #[error("Waiting for other clients to reconnect.")]
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

/// Run a source Development Server whose supervisor pipe owns its lifetime.
/// This never provisions an artifact, registers a service, or changes Stop intent.
pub fn run_owned_development(
    panes: impl pane::PaneService + 'static,
    owner: impl std::io::Read + Send + 'static,
    ready: impl FnOnce() -> Result<(), ServerError>,
) -> Result<(), ServerError> {
    host::serve_owned(
        &host::Paths::for_identity("spaceterm-development")?,
        Box::new(panes),
        owner,
        ready,
    )
}
