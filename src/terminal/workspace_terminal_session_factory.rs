use std::path::PathBuf;
use std::rc::Rc;

use super::geometry::TerminalGeometry;
use super::session::{SessionError, StartedTerminalSession, TerminalSessionFactory};

#[derive(Clone)]
pub(crate) struct WorkspaceTerminalSessionFactory {
    session_factory: Rc<dyn TerminalSessionFactory>,
    workspace_root: PathBuf,
}

impl WorkspaceTerminalSessionFactory {
    pub(crate) fn new(
        session_factory: Rc<dyn TerminalSessionFactory>,
        workspace_root: PathBuf,
    ) -> Self {
        Self {
            session_factory,
            workspace_root,
        }
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
}
