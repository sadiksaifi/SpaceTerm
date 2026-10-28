use std::rc::Rc;

use super::geometry::TerminalGeometry;
use super::metadata::TerminalLocalFileCapabilities;
use super::session::{
    LocalTerminalLaunchPlan, StartedTerminalSession, TerminalSessionError, TerminalSessionFactory,
};
use crate::domain::ValidatedLocalDirectory;

#[derive(Clone)]
/// Starts client presentations of Terminal Sessions owned by a SpaceTerm Server.
pub(crate) struct WorkspaceTerminalSessionFactory {
    session_factory: Option<Rc<dyn TerminalSessionFactory>>,
    launch_plan: LocalTerminalLaunchPlan,
    /// The title a Pane without a Terminal Session reads, if not the generic one.
    pending_title: Option<String>,
}

impl WorkspaceTerminalSessionFactory {
    #[cfg(test)]
    pub(crate) fn new_local(
        session_factory: Rc<dyn TerminalSessionFactory>,
        home_directory: ValidatedLocalDirectory,
    ) -> Self {
        Self::server_shaped(session_factory, home_directory)
    }

    /// The owning server selects the directory. The local placeholder carries no authority.
    pub(crate) fn new_server(
        session_factory: Rc<dyn TerminalSessionFactory>,
        home_directory: ValidatedLocalDirectory,
    ) -> Self {
        debug_assert!(session_factory.is_server_backed());
        Self::server_shaped(session_factory, home_directory)
    }

    fn server_shaped(
        session_factory: Rc<dyn TerminalSessionFactory>,
        home_directory: ValidatedLocalDirectory,
    ) -> Self {
        Self {
            session_factory: Some(session_factory),
            launch_plan: LocalTerminalLaunchPlan::new(home_directory),
            pending_title: None,
        }
    }

    /// An optimistic Pane owns no Terminal Session or server identity yet.
    pub(crate) fn pending(home_directory: ValidatedLocalDirectory) -> Self {
        Self {
            session_factory: None,
            launch_plan: LocalTerminalLaunchPlan::new(home_directory),
            pending_title: None,
        }
    }

    /// A Pane of a server this client is not connected to. It reads `title`, the title this
    /// client retained for it, and owns no Terminal Session.
    pub(crate) fn unavailable(
        home_directory: ValidatedLocalDirectory,
        title: Option<String>,
    ) -> Self {
        Self {
            pending_title: title,
            ..Self::pending(home_directory)
        }
    }

    pub(crate) fn is_pending(&self) -> bool {
        self.session_factory.is_none()
    }

    pub(crate) fn start(
        &self,
        geometry: TerminalGeometry,
        initial_appearance: super::TerminalAppearanceUpdate,
    ) -> Result<StartedTerminalSession, TerminalSessionError> {
        self.session_factory
            .as_ref()
            .ok_or(TerminalSessionError::SpawnWorker(
                std::io::ErrorKind::WouldBlock,
            ))?
            .start(geometry, self.launch_plan.clone(), initial_appearance)
    }

    pub(crate) fn fallback_title(&self) -> String {
        self.session_factory.as_ref().map_or_else(
            || {
                self.pending_title
                    .clone()
                    .unwrap_or_else(|| "Terminal".to_owned())
            },
            |factory| factory.fallback_title(),
        )
    }

    /// Remote server metadata never grants client-local filesystem authority.
    pub(crate) fn local_file_capabilities(&self) -> TerminalLocalFileCapabilities {
        if self.allows_permission_setup() {
            TerminalLocalFileCapabilities::Enabled
        } else {
            TerminalLocalFileCapabilities::Disabled
        }
    }

    pub(crate) fn allows_permission_setup(&self) -> bool {
        self.session_factory
            .as_ref()
            .is_some_and(|factory| factory.local_file_authority().is_some())
    }

    pub(crate) fn file_insertion_capabilities(&self) -> TerminalLocalFileCapabilities {
        if self
            .session_factory
            .as_ref()
            .is_some_and(|factory| factory.allows_local_file_insertion())
        {
            TerminalLocalFileCapabilities::Enabled
        } else {
            self.local_file_capabilities()
        }
    }

    pub(crate) fn is_server_backed(&self) -> bool {
        self.session_factory
            .as_ref()
            .is_none_or(|factory| factory.is_server_backed())
    }
}
