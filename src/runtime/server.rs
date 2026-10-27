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

    pub(crate) fn directory_host() -> impl FnMut(&Request) -> Reply + Send {
        let mut host = terminal_host();
        move |request| host.request([3; 32], [4; 32], request.clone())
    }
}

#[cfg(test)]
mod directory_tests;
mod dispatch;
#[cfg(test)]
#[path = "server/effect_tests.rs"]
mod effect_tests;
mod effects;
use self::effects::NativeEffects;
mod graphics;
#[cfg(test)]
mod graphics_tests;
mod metadata;
#[cfg(test)]
mod metadata_tests;
mod pane_host;
#[cfg(test)]
mod pane_host_tests;
mod reading;
#[cfg(test)]
mod reading_tests;
mod repositories;
#[cfg(test)]
#[path = "server/repository_tests.rs"]
mod repository_tests;
mod screen;
#[cfg(test)]
mod screen_tests;
#[allow(
    dead_code,
    reason = "the push connection dispatches ScrollbackPage once transport and push merge"
)]
mod scrollback;
mod shared;
pub use shared::SharedWorkspaceHost;
#[cfg(test)]
mod tests;

use self::pane_host::decode_modifiers;
pub(crate) use self::pane_host::{decode_key, geometry};
use self::screen::appearance;
#[cfg(test)]
use self::screen::encode_screen;

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
}
#[derive(Clone, Copy)]
struct DormantPane {
    device: Option<[u8; 32]>,
    attachment_generation: Option<u64>,
    control_epoch: u64,
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
    sent: Option<PresentationGeneration>,
    screen_encoder: pane::presentation::ScreenEncoder,
    presentable: bool,
    ended: bool,
    failed: bool,
    directory_failed: bool,
    ended_restart: bool,
    appearance_generation: u64,
    focus_generation: u64,
    focus: FocusTarget,
    pending_paste: Option<u64>,
    screen_generation: u64,
    size: PaneSize,
    effects: NativeEffects,
    reading: reading::Reading,
    accessibility: accessibility::Accessibility,
}
fn nonce() -> Result<[u8; 32], ServerError> {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).map_err(|_| ServerError::StartupFailed)?;
    Ok(bytes)
}
impl DevelopmentPaneHost {
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
        })
    }
}
