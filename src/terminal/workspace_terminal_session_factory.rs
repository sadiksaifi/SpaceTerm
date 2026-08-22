use std::io::ErrorKind;
use std::path::PathBuf;
use std::rc::Rc;

use super::geometry::TerminalGeometry;
use super::session::{SessionError, StartedTerminalSession, TerminalSessionFactory};

/// Resolves the directory each Terminal Session starts in. Sources may be
/// fixed or resolved again at every session start.
type DirectoryResolver = Rc<dyn Fn() -> Option<PathBuf>>;

#[derive(Clone)]
enum WorkspaceDirectoryOrigin {
    Fixed(PathBuf),
    Dynamic(DirectoryResolver),
}

#[derive(Clone)]
pub(crate) struct WorkspaceTerminalSessionFactory {
    session_factory: Rc<dyn TerminalSessionFactory>,
    directory_origin: WorkspaceDirectoryOrigin,
}

impl WorkspaceTerminalSessionFactory {
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "only test harnesses still bind a fixed Workspace root"
        )
    )]
    pub(crate) fn new(
        session_factory: Rc<dyn TerminalSessionFactory>,
        workspace_root: PathBuf,
    ) -> Self {
        Self {
            session_factory,
            directory_origin: WorkspaceDirectoryOrigin::Fixed(workspace_root),
        }
    }

    /// Binds a source whose directory is resolved again at every session
    /// start, so new Windows and Panes begin in the current Workspace
    /// Directory. `None` means the Workspace Directory is currently
    /// unavailable; creation flows must gate on the same source before
    /// creating entities.
    pub(crate) fn dynamic(
        session_factory: Rc<dyn TerminalSessionFactory>,
        resolve_directory: impl Fn() -> Option<PathBuf> + 'static,
    ) -> Self {
        Self {
            session_factory,
            directory_origin: WorkspaceDirectoryOrigin::Dynamic(Rc::new(resolve_directory)),
        }
    }

    fn resolve_start_directory(&self) -> Option<PathBuf> {
        match &self.directory_origin {
            WorkspaceDirectoryOrigin::Fixed(directory) => Some(directory.clone()),
            WorkspaceDirectoryOrigin::Dynamic(resolve_directory) => resolve_directory(),
        }
    }

    pub(crate) fn start(
        &self,
        geometry: TerminalGeometry,
    ) -> Result<StartedTerminalSession, SessionError> {
        let working_directory = match self.resolve_start_directory() {
            Some(working_directory) => working_directory,
            None => {
                eprintln!(
                    "cannot start a Terminal Session: the Workspace directory is unavailable"
                );
                return Err(SessionError::SpawnWorker(std::io::Error::new(
                    ErrorKind::NotFound,
                    "the Workspace directory is unavailable",
                )));
            }
        };
        self.session_factory.start(geometry, &working_directory)
    }

    pub(crate) fn fallback_title(&self) -> String {
        self.session_factory.fallback_title()
    }
}
