//! SpaceTerm Protocol admission, direct-network access and account server setup.
pub mod attachments;
#[cfg(feature = "client")]
mod client;
pub mod client_view;
#[cfg(feature = "client")]
mod computer_name;
pub mod connection;
pub mod direct;
pub mod hierarchy;
mod host;
pub mod labels;
pub mod organization;
#[cfg(feature = "client")]
pub mod organize;
pub mod pane;
pub mod permissions;
pub mod protocol;
pub mod provisioning;
pub mod repository;
pub mod service;
#[cfg(feature = "client")]
mod startup;
mod storage;
pub mod workspace_order;
pub mod worktree;

#[cfg(feature = "client")]
pub use client::{CliRoute, ClientScope, ServerConnection};
#[cfg(feature = "client")]
pub use startup::{LocalStartup, StartupOutcome};
pub use storage::{Directory, Lock, LockWait};

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
    /// Another service operation, such as Update, Restart, Stop or Start, holds the service.
    /// Launch reports this while it runs. Launch and admission also report it when the
    /// operation outlasts their wait.
    #[error("Another server operation is running. Try again when it finishes.")]
    ServiceBusy,
    #[error("Install the server service first.")]
    NotInstalled,
    #[error("The local server was stopped. Use Start Server to start it again.")]
    Stopped,
    #[error("The server changed. Review and confirm again.")]
    Changed,
    #[error(
        "SpaceTerm couldn\u{2019}t confirm whether the server update finished. Check the update in Server Settings."
    )]
    ActivationUnresolved,
    #[error("SpaceTerm can\u{2019}t safely restore the previous server version.")]
    ReturnUnavailable,
    #[error("Couldn\u{2019}t verify this server package.")]
    RuntimeUnavailable,
    /// This build includes no server package to verify. Distinct from a package that failed
    /// verification.
    #[error("This SpaceTerm build doesn\u{2019}t include a server package.")]
    CandidateMissing,
    #[error("Administrator authorization required.")]
    AuthorizationRequired,
    #[error("The server is not running.")]
    NotRunning,
    #[error("Server storage is unavailable.")]
    StorageUnavailable,
    #[error("This server\u{2019}s saved state is from an incompatible SpaceTerm.")]
    IncompatibleState,
    #[error("Server storage has unsafe permissions.")]
    UnsafeStorage,
    #[error("SpaceTerm couldn\u{2019}t verify the server or account for this connection.")]
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
    #[error("This operation isn\u{2019}t supported.")]
    Unsupported,
    #[error("Directory unavailable.")]
    DirectoryUnavailable,
    #[error("Couldn\u{2019}t confirm the Terminal Sessions ended.")]
    CleanupUnresolved,
    #[error("Waiting for other clients to reconnect.")]
    Capacity,
}

/// The CLI's state root, `clients/cli` under the account-private server data of `identity`.
/// It shares no file with the desktop's `clients/desktop`.
pub fn cli_state_root(identity: &str) -> Result<std::path::PathBuf, ServerError> {
    Ok(host::Paths::for_identity(identity)?
        .data
        .join("clients/cli"))
}

/// The nonsecret ID of the server `identity` on this account, read without contacting or
/// starting it.
pub fn local_server_id(identity: &str) -> Result<[u8; 32], ServerError> {
    Ok(
        storage::Directory::open(&host::Paths::for_identity(identity)?.data, false)?
            .read()?
            .server,
    )
}

/// Runs an explicitly requested local server. No service registration or listener
/// is installed by library construction or desktop launch.
pub fn run_local(identity: &str) -> Result<(), ServerError> {
    host::install_shutdown_signals()?;
    let paths = host::Paths::for_identity(identity)?;
    host::serve(&paths)
}

/// Builds the service once the host has its one [`connection::sync::SyncWorker`], which the
/// service shares for its disk syncs.
pub type PaneFactory = Box<
    dyn FnOnce(
        connection::sync::SyncWorker,
    ) -> Result<Box<dyn connection::service::PaneService>, ServerError>,
>;

/// A [`PaneFactory`] for `build`.
pub fn panes<P: connection::service::PaneService + 'static>(
    build: impl FnOnce(connection::sync::SyncWorker) -> Result<P, ServerError> + 'static,
) -> PaneFactory {
    Box::new(move |sync| {
        build(sync).map(|panes| Box::new(panes) as Box<dyn connection::service::PaneService>)
    })
}

pub fn run_local_with_panes(identity: &str, panes: PaneFactory) -> Result<(), ServerError> {
    host::install_shutdown_signals()?;
    host::serve_with_panes(&host::Paths::for_identity(identity)?, Some(panes))
}

/// Run a source Development Server whose supervisor pipe owns its lifetime.
/// This never provisions an artifact, registers a service, or changes Stop intent.
pub fn run_owned_development(
    panes: PaneFactory,
    owner: impl std::io::Read + Send + 'static,
    ready: impl FnOnce() -> Result<(), ServerError>,
) -> Result<(), ServerError> {
    host::serve_owned(
        &host::Paths::for_identity("spaceterm-development")?,
        panes,
        owner,
        ready,
    )
}
