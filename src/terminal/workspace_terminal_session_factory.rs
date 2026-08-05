use std::path::PathBuf;
use std::rc::Rc;

use super::session::{GridSize, SessionError, StartedTerminalSession, TerminalSessionFactory};

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

    pub(crate) fn start(&self, size: GridSize) -> Result<StartedTerminalSession, SessionError> {
        self.session_factory.start(size, &self.workspace_root)
    }

    pub(crate) fn fallback_title(&self) -> String {
        self.session_factory.fallback_title()
    }
}
