//! SpaceTerm Protocol admission, direct-network access and account server setup.
pub mod attachments;
#[cfg(feature = "client")]
mod client;
pub mod client_view;
#[cfg(feature = "client")]
mod computer_name;
pub mod connection;
pub mod diagnostics;
pub mod direct;
pub mod hierarchy;
mod host;
pub mod labels;
#[cfg(target_os = "macos")]
mod launch_services;
pub mod migration;
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
pub mod update;
pub mod workspace_order;
pub mod worktree;

#[cfg(feature = "client")]
pub use client::{CliRoute, ClientScope, MachineConnection};
#[cfg(feature = "client")]
pub use startup::{LocalStartup, StartupOutcome};
pub use storage::{Directory, Lock, LockWait};

/// An admitted work connection's negotiated protocol version.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuthenticatedStatus {
    pub work: protocol::version::ProtocolVersion,
}

/// Bounded classifications. Native errors and peer-supplied values never cross this boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CoreError {
    #[error("{0}")]
    Direct(crate::direct::DirectError),
    #[error("SpaceTerm Core is not set up.")]
    NotSetUp,
    #[error("SpaceTerm Core service unavailable. Sign in and try again.")]
    ServiceUnavailable,
    /// Another service operation, such as Update, Restart, Stop or Start, holds the service.
    /// Launch reports this while it runs. Launch and admission also report it when the
    /// operation outlasts their wait.
    #[error("Another SpaceTerm Core operation is running. Try again when it finishes.")]
    ServiceBusy,
    #[error("Install the SpaceTerm Core service first.")]
    NotInstalled,
    #[error("The local SpaceTerm Core was stopped. Use Start SpaceTerm Core to start it again.")]
    Stopped,
    /// The service's background item is off in Login Items & Extensions, because the person
    /// turned it off or has not approved it yet. The service manager won't run it, and only the
    /// person turns it on there.
    #[error(
        "SpaceTerm Core is off in Login Items. Turn it on in System Settings > General > Login Items & Extensions."
    )]
    LoginItemOff,
    #[error("SpaceTerm Core changed. Review and confirm again.")]
    Changed,
    #[error(
        "SpaceTerm couldn\u{2019}t confirm whether the SpaceTerm Core update finished. Check the update in Machine Settings."
    )]
    ActivationUnresolved,
    #[error("Couldn\u{2019}t verify this SpaceTerm Core package.")]
    RuntimeUnavailable,
    /// This build includes no server package to verify. Distinct from a package that failed
    /// verification.
    #[error("This SpaceTerm build doesn\u{2019}t include a SpaceTerm Core package.")]
    CandidateMissing,
    #[error("Administrator authorization required.")]
    AuthorizationRequired,
    #[error("SpaceTerm Core is not running.")]
    NotRunning,
    #[error("SpaceTerm Core storage is unavailable.")]
    StorageUnavailable,
    #[error("This SpaceTerm Core\u{2019}s saved state is from an incompatible SpaceTerm.")]
    IncompatibleState,
    /// The Core's data is older than any generation it migrates, or its migration failed. The
    /// runtime refuses to read it until the person resets it or installs a Stepping Stone.
    #[error("SpaceTerm Core\u{2019}s saved data needs a reset before it can start.")]
    DataResetRequired,
    #[error("SpaceTerm Core storage has unsafe permissions.")]
    UnsafeStorage,
    #[error("SpaceTerm couldn\u{2019}t verify the machine or account for this connection.")]
    AuthenticationRefused,
    /// The two sides share no protocol major. It names the older side.
    #[error("SpaceTerm and this SpaceTerm Core are incompatible.")]
    IncompatibleProtocol(protocol::version::Incompatible),
    #[error(
        "SpaceTerm was updated or moved after it opened. Quit and reopen SpaceTerm to continue."
    )]
    CandidateChanged,
    #[error("The connection to SpaceTerm Core did not complete.")]
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
pub fn cli_state_root(identity: &str) -> Result<std::path::PathBuf, CoreError> {
    Ok(host::Paths::for_identity(identity)?
        .data
        .join("clients/cli"))
}

/// SpaceTerm Core's data root of `identity` on this account, under the account's own home
/// directory whatever HOME says.
pub fn core_data_root(identity: &str) -> Result<std::path::PathBuf, CoreError> {
    Ok(host::Paths::for_identity(identity)?.data)
}

/// The nonsecret ID of the server `identity` on this account, read without contacting or
/// starting it.
pub fn local_machine_id(identity: &str) -> Result<[u8; 32], CoreError> {
    Ok(
        storage::Directory::open(&host::Paths::for_identity(identity)?.data, false)?
            .read()?
            .core,
    )
}

/// Builds the service once the host has its one [`connection::sync::SyncWorker`], which the
/// service shares for its disk syncs, and carries the Terminal Diagnostics counters the service
/// records into, which the lane reports.
pub struct PaneFactory {
    build: BuildPanes,
    counters: std::sync::Arc<diagnostics::TerminalCounters>,
    version: Option<update::facts::Version>,
    client_data: Option<Box<dyn FnOnce() -> Result<(), CoreError>>>,
}

impl PaneFactory {
    /// Brings the data root of a client this run's installation ships to its generation before
    /// the run reports readiness, after this Core's own data. A refusal ends the start unready.
    pub fn with_client_data(
        mut self,
        prepare: impl FnOnce() -> Result<(), CoreError> + 'static,
    ) -> Self {
        self.client_data = Some(Box::new(prepare));
        self
    }
    /// Names this run's release version, which admission offers to every client. Without it,
    /// the run offers [`UNVERSIONED`].
    pub fn with_version(mut self, version: update::facts::Version) -> Self {
        self.version = Some(version);
        self
    }
}

/// The version a Core run reports when its host named none, as tests and examples do.
pub const UNVERSIONED: &str = "0.0.0";

type BuildPanes = Box<
    dyn FnOnce(
        connection::sync::SyncWorker,
    ) -> Result<Box<dyn connection::service::PaneService>, CoreError>,
>;

/// A [`PaneFactory`] for `build`, whose service records into `counters`.
pub fn panes<P: connection::service::PaneService + 'static>(
    counters: std::sync::Arc<diagnostics::TerminalCounters>,
    build: impl FnOnce(connection::sync::SyncWorker) -> Result<P, CoreError> + 'static,
) -> PaneFactory {
    PaneFactory {
        build: Box::new(move |sync| {
            build(sync).map(|panes| Box::new(panes) as Box<dyn connection::service::PaneService>)
        }),
        counters,
        version: None,
        client_data: None,
    }
}

/// Runs an explicitly requested local server. No service registration or listener
/// is installed by library construction or desktop launch.
pub fn run_local_with_panes(identity: &str, panes: PaneFactory) -> Result<(), CoreError> {
    host::install_shutdown_signals()?;
    host::serve_with_panes(&host::Paths::for_identity(identity)?, Some(panes))
}

/// Run a source Development Server whose supervisor pipe owns its lifetime.
/// This never provisions an artifact, registers a service, or changes Stop intent.
pub fn run_owned_development(
    panes: PaneFactory,
    owner: impl std::io::Read + Send + 'static,
    ready: impl FnOnce() -> Result<(), CoreError>,
) -> Result<(), CoreError> {
    host::serve_owned(
        &host::Paths::for_identity("spaceterm-development")?,
        panes,
        owner,
        ready,
    )
}
