//! The first retained Development Workspace and its independently owned live Terminal Session.
use super::*;
use crate::terminal::session::TerminalSessionStartupStage;
use spaceterm_server::organization::{Organization, OrganizationStore};
use spaceterm_server::{
    ServerError,
    pane::{self, *},
};
use std::{sync::Arc, time::Duration};

mod accessibility;
#[cfg(test)]
mod accessibility_tests;
#[cfg(test)]
mod boundary_tests;
#[cfg(test)]
mod clear_tests;
mod directories;
#[cfg(all(test, feature = "desktop"))]
pub(crate) mod testing {
    use super::*;

    pub(crate) fn terminal_host() -> SharedWorkspaceHost {
        SharedWorkspaceHost::new(super::tests::host())
    }

    /// A server's directory reads and creations, in process, for one client connection.
    pub(crate) struct DirectoryHost {
        host: SharedWorkspaceHost,
        /// Rung when a read settles.
        woken: std::sync::mpsc::Receiver<()>,
        settled: Vec<SettledRead>,
    }

    pub(crate) fn directory_host() -> DirectoryHost {
        let mut host = terminal_host();
        let (sender, woken) = std::sync::mpsc::channel();
        host.ticker(Arc::new(move || {
            let _ = sender.send(());
        }));
        DirectoryHost {
            host,
            woken,
            settled: Vec::new(),
        }
    }

    impl DirectoryHost {
        const DEVICE: [u8; 32] = [3; 32];
        const CONNECTION: [u8; 32] = [4; 32];

        /// Starts a directory read or creation as request `id`.
        pub(crate) fn begin(
            &mut self,
            id: u64,
            operation: spaceterm_server::protocol::Operation,
        ) -> Result<(), ()> {
            use spaceterm_server::protocol::Operation;
            let read = match operation {
                Operation::DirectoryRead(operation) => HostRead::Directory(operation),
                Operation::DirectoryCreate(creation) => HostRead::Creation(creation),
                _ => return Err(()),
            };
            let id = request_id(id)?;
            if let Some(reply) = self.host.read(Self::DEVICE, Self::CONNECTION, id, read) {
                self.settled.push(SettledRead {
                    connection: Self::CONNECTION,
                    id,
                    reply: reply.map_err(|_| ())?,
                });
            }
            Ok(())
        }

        /// Waits at most `timeout` for request `id` to settle.
        pub(crate) fn settled(
            &mut self,
            id: u64,
            timeout: Duration,
        ) -> Result<Option<Result<pane::directories::Value, pane::directories::Error>>, ()>
        {
            let id = request_id(id)?;
            let deadline = std::time::Instant::now() + timeout;
            loop {
                self.settled.extend(self.host.settled_reads());
                if let Some(index) = self.settled.iter().position(|read| read.id == id) {
                    return match self.settled.swap_remove(index).reply {
                        spaceterm_server::protocol::ReplyBody::Directory(result) => {
                            Ok(Some(result))
                        }
                        _ => Err(()),
                    };
                }
                let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now())
                else {
                    return Ok(None);
                };
                let _ = self.woken.recv_timeout(remaining);
            }
        }

        pub(crate) fn cancel(&mut self, id: u64) {
            if let Ok(id) = request_id(id) {
                self.host.cancel_read(Self::CONNECTION, id);
            }
        }
    }

    fn request_id(id: u64) -> Result<spaceterm_server::protocol::RequestId, ()> {
        u32::try_from(id)
            .map(spaceterm_server::protocol::RequestId)
            .map_err(|_| ())
    }
}

#[cfg(test)]
mod directory_tests;
mod dispatch;
pub use dispatch::{HostCommands, HostRead, Observers, RunRequest, Served, SettledRead};
pub use publish::{Observation, Observed};
pub use screen_encoder::ScreenCapture;
#[cfg(test)]
#[path = "server/effect_tests.rs"]
mod effect_tests;
mod effects;
use self::effects::NativeEffects;
#[cfg(test)]
mod agent_io_tests;
mod graphics;
#[cfg(test)]
mod graphics_tests;
#[cfg(test)]
mod input_tests;
mod metadata;
#[cfg(test)]
mod metadata_tests;
mod observe;
mod pane_host;
#[cfg(test)]
mod pane_host_tests;
mod publish;
mod repositories;
#[cfg(test)]
#[path = "server/repository_tests.rs"]
mod repository_tests;
mod screen;
pub(crate) mod screen_encoder;
#[cfg(test)]
mod screen_encoder_tests;
#[cfg(test)]
mod screen_tests;
mod scrollback;
pub use self::scrollback::{PageReply, ScrollbackRead};
#[cfg(test)]
mod served_tests;
mod shared;
pub use shared::SharedWorkspaceHost;
#[cfg(test)]
mod tests;

pub(crate) use self::pane_host::{decode_key, geometry};
use self::screen::appearance;

pub struct DevelopmentPaneHost {
    boot: [u8; 32],
    pane: Option<OwnedPane>,
    factory: NativeTerminalSessionFactory,
    directory: std::path::PathBuf,
    organization: Option<Organization>,
    store: Option<OrganizationStore>,
    uncertain: bool,
    directory_unavailable: bool,
    failed_directory_session: Option<PaneTarget>,
    dormant: Option<DormantPane>,
    directories: directories::DirectoryOperations,
    /// Shared by every Pane of one server, so each names its Controlling Client the same way.
    names: Arc<std::sync::Mutex<control::ClientNames>>,
    /// Makes each started Terminal Session's notifier, which wakes the connections presenting it.
    notifier: Option<dispatch::SessionNotifiers>,
    /// What each started Terminal Session tells the CLI inside it about this server.
    session_context: Option<Arc<ServerSessionContext>>,
}

/// What every Terminal Session of one server tells `spaceterm` inside it: the server's build
/// identity and ID, and the server's own CLI executable.
#[derive(Clone)]
pub struct ServerSessionContext {
    pub identity: String,
    pub server: [u8; 32],
    pub cli: std::path::PathBuf,
}

impl ServerSessionContext {
    /// The context of the server `identity` that this process runs, whose executable is its
    /// own CLI.
    pub fn current(identity: &str) -> Result<Self, ServerError> {
        Ok(Self {
            identity: identity.to_owned(),
            server: spaceterm_server::local_server_id(identity)?,
            cli: std::env::current_exe().map_err(|_| ServerError::StartupFailed)?,
        })
    }
}
/// A Pane the running server keeps after its Session ended, with the control grant it had.
#[derive(Clone, Copy)]
struct DormantPane {
    device: Option<[u8; 32]>,
    attachment_generation: Option<u64>,
    control_epoch: u64,
    end: pane::SessionEnd,
    /// The Session that ended, so its channel learns that the Pane stays.
    session: Option<PaneTarget>,
}
struct OwnedPane {
    device: Option<[u8; 32]>,
    attachment_generation: Option<u64>,
    /// Public grant generation. Unlike lease.control, this grants no terminal authority.
    control_epoch: u64,
    lease: Lease,
    connected: bool,
    runtime: TerminalRuntime,
    subscription: TerminalSubscription,
    screen: Option<Arc<ScreenSnapshot>>,
    graphics_transfer: Option<(ActiveScreenSnapshot, Arc<crate::terminal::ImageSnapshot>)>,
    /// Whether the controlling client shows this Pane, so its Terminal Session publishes frames.
    presentable: bool,
    /// Whether an observer shows this Pane, which also makes its Terminal Session publish.
    observed: bool,
    ended: bool,
    /// Why the shell's exit keeps the Pane. `None` with `ended` closes the Pane.
    end: Option<pane::SessionEnd>,
    failed: bool,
    directory_failed: bool,
    /// Why the Session this one replaced ended, so a failed start leaves the Pane ended again.
    ended_restart: Option<pane::SessionEnd>,
    appearance_generation: u64,
    focus_generation: u64,
    focus: FocusTarget,
    pending_paste: Option<u64>,
    screen_generation: u64,
    size: PaneSize,
    effects: NativeEffects,
    accessibility: accessibility::Accessibility,
    /// The Terminal Metadata revision last reported by [`DevelopmentPaneHost::metadata_changed`].
    metadata_revision: u64,
}
fn nonce() -> Result<[u8; 32], ServerError> {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).map_err(|_| ServerError::StartupFailed)?;
    Ok(bytes)
}
impl DevelopmentPaneHost {
    /// This Pane's running Terminal Session, if one has not ended.
    pub(crate) fn live_session(&self) -> Option<[u8; 32]> {
        self.pane
            .as_ref()
            .filter(|pane| !pane.ended)
            .map(|pane| pane.lease.session)
    }
    pub fn new(
        factory: NativeTerminalSessionFactory,
        directory: ValidatedLocalDirectory,
    ) -> Result<Self, ServerError> {
        Ok(Self {
            boot: nonce()?,
            pane: None,
            factory,
            directory: directory.path().to_owned(),
            organization: None,
            store: None,
            uncertain: false,
            directory_unavailable: false,
            failed_directory_session: None,
            dormant: None,
            directories: directories::DirectoryOperations::default(),
            names: Arc::default(),
            notifier: None,
            session_context: None,
        })
    }
    /// Native composition for the explicitly launched local Development host.
    pub fn local() -> Result<Self, ServerError> {
        Self::local_with_pty_factory(native_pty_factory())
    }

    fn local_with_pty_factory(
        pty_factory: Arc<dyn NativePtyAdapterFactory>,
    ) -> Result<Self, ServerError> {
        let filesystem = local_filesystem();
        let directory = crate::platform::app_directories::AppDirectoryEnvironment::capture()
            .home
            .map(std::path::PathBuf::from)
            .ok_or(ServerError::StartupFailed)?;
        let factory = NativeTerminalSessionFactory::new(
            pty_factory,
            crate::platform::launch_host::shell_launch_planner(),
            filesystem,
            crate::platform::launch_host::terminal_machine(&directory),
        );
        Ok(Self {
            boot: nonce()?,
            pane: None,
            factory,
            directory,
            organization: None,
            store: None,
            uncertain: false,
            directory_unavailable: false,
            failed_directory_session: None,
            dormant: None,
            directories: directories::DirectoryOperations::default(),
            names: Arc::default(),
            notifier: None,
            session_context: None,
        })
    }
}
