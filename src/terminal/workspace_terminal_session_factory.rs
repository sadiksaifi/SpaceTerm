use std::rc::Rc;

use super::geometry::TerminalGeometry;
use super::metadata::{CurrentDirectory, TerminalLocalFileCapabilities};
use super::session::{
    LocalTerminalLaunchPlan, StartedTerminalSession,
    TerminalSessionError, TerminalSessionFactory,
};
use crate::domain::{PinnedDirectory, ValidatedLocalDirectory};
use crate::platform::local_filesystem::LocalFilesystemError;

#[derive(Debug)]
/// A launch request for a Pane owned by a SpaceTerm Server.
pub(crate) struct PreparedWorkspaceTerminalLaunch {
    launch_plan: LocalTerminalLaunchPlan,
}

impl PreparedWorkspaceTerminalLaunch {
    pub(crate) fn starting_directory(&self) -> CurrentDirectory {
        let plan = &self.launch_plan;
        CurrentDirectory::Local(plan.working_directory().path().to_owned())
    }
}

#[derive(Clone)]
/// Starts client presentations of Terminal Sessions owned by a SpaceTerm Server.
pub(crate) struct WorkspaceTerminalSessionFactory {
    session_factory: Rc<dyn TerminalSessionFactory>,
    launch_plan: LocalTerminalLaunchPlan,
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
            session_factory,
            launch_plan: LocalTerminalLaunchPlan::new(home_directory),
        }
    }

    pub(crate) fn prepare_child_launch(
        &self,
    ) -> Result<PreparedWorkspaceTerminalLaunch, super::TerminalSessionChannelUnavailable> {
        Ok(PreparedWorkspaceTerminalLaunch {
            launch_plan: self.launch_plan.clone(),
        })
    }

    pub(crate) fn start(
        &self,
        geometry: TerminalGeometry,
        prepared_launch: PreparedWorkspaceTerminalLaunch,
        initial_appearance: super::TerminalAppearanceUpdate,
    ) -> Result<StartedTerminalSession, TerminalSessionError> {
        self.session_factory
            .start(geometry, prepared_launch.launch_plan, initial_appearance)
    }

    pub(crate) fn fallback_title(&self) -> String {
        self.session_factory.fallback_title()
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
        self.session_factory.local_file_authority().is_some()
    }

    pub(crate) fn file_insertion_capabilities(&self) -> TerminalLocalFileCapabilities {
        if self.session_factory.allows_local_file_insertion() {
            TerminalLocalFileCapabilities::Enabled
        } else {
            self.local_file_capabilities()
        }
    }

    pub(crate) fn is_server_backed(&self) -> bool {
        self.session_factory.is_server_backed()
    }

    #[cfg(test)]
    pub(crate) fn local_working_directory(&self) -> Option<&std::path::Path> {
        Some(self.launch_plan.working_directory().path())
    }

    pub(crate) fn set_pinned_directory(&mut self, _directory: Option<PinnedDirectory>) {}

    pub(crate) fn for_source_directory(
        &self,
        _source: Option<CurrentDirectory>,
    ) -> Result<Self, LocalFilesystemError> {
        Ok(self.clone())
    }
}
