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
pub mod idle_restart;
pub mod labels;
#[cfg(target_os = "macos")]
mod launch_services;
pub mod migration;
#[cfg(feature = "client")]
pub mod native_client;
pub mod organization;
#[cfg(feature = "client")]
pub mod organize;
pub mod pane;
pub mod permissions;
pub mod protocol;
pub mod provisioning;
pub mod repository;
#[cfg(feature = "client")]
mod retry;
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
pub use startup::{LaunchIntent, LocalStartup, StartupOutcome};
pub use storage::{Directory, Lock, LockWait};

pub use spaceterm_client::{AuthenticatedStatus, CoreError};

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
