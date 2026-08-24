use std::path::PathBuf;
use std::rc::Rc;

use super::geometry::TerminalGeometry;
use super::session::{SessionError, StartedTerminalSession, TerminalSessionFactory};
use crate::domain::{ValidatedWorkspaceDirectory, WorkspaceDirectoryIdentity};
use crate::platform::workspace_directory::{WorkspaceDirectoryError, validate_workspace_directory};

#[derive(Clone)]
pub(crate) struct WorkspaceTerminalSessionFactory {
    session_factory: Rc<dyn TerminalSessionFactory>,
    workspace_root: PathBuf,
    expected_identity: Option<WorkspaceDirectoryIdentity>,
}

impl WorkspaceTerminalSessionFactory {
    pub(crate) fn new(
        session_factory: Rc<dyn TerminalSessionFactory>,
        workspace_root: PathBuf,
    ) -> Self {
        Self {
            session_factory,
            workspace_root,
            expected_identity: None,
        }
    }

    pub(crate) fn with_directory_identity(mut self, identity: WorkspaceDirectoryIdentity) -> Self {
        self.expected_identity = Some(identity);
        self
    }

    pub(crate) fn start(
        &self,
        geometry: TerminalGeometry,
    ) -> Result<StartedTerminalSession, SessionError> {
        self.session_factory.start(geometry, &self.workspace_root)
    }

    pub(crate) fn fallback_title(&self) -> String {
        self.session_factory.fallback_title()
    }

    pub(crate) fn working_directory(&self) -> &std::path::Path {
        &self.workspace_root
    }

    pub(crate) fn validate_working_directory(
        &self,
    ) -> Result<ValidatedWorkspaceDirectory, WorkspaceDirectoryError> {
        #[cfg(test)]
        if self.expected_identity.is_none()
            || self
                .expected_identity
                .is_some_and(WorkspaceDirectoryIdentity::is_synthetic)
        {
            let identity = self
                .expected_identity
                .unwrap_or_else(|| WorkspaceDirectoryIdentity::new(0, 0));
            return Ok(ValidatedWorkspaceDirectory::new(
                self.workspace_root.clone(),
                identity,
            ));
        }
        let directory = validate_workspace_directory(&self.workspace_root)?;
        if self
            .expected_identity
            .is_some_and(|identity| identity != directory.identity())
        {
            return Err(WorkspaceDirectoryError::IdentityChanged);
        }
        Ok(directory)
    }

    pub(crate) fn set_working_directory(
        &mut self,
        workspace_root: PathBuf,
        identity: WorkspaceDirectoryIdentity,
    ) {
        self.workspace_root = workspace_root;
        self.expected_identity = Some(identity);
    }
}
