use std::rc::Rc;

use super::geometry::TerminalGeometry;
use super::metadata::TerminalLocalFileCapabilities;
use super::session::{StartedTerminalSession, TerminalSessionError, TerminalSessionFactory};

#[derive(Clone)]
/// Starts client presentations of Terminal Sessions owned by a SpaceTerm Server.
pub(crate) struct WorkspaceTerminalSessionFactory {
    session_factory: Option<Rc<dyn TerminalSessionFactory>>,
    /// The title a Pane without a Terminal Session reads, if not the generic one.
    pending_title: Option<String>,
}

impl WorkspaceTerminalSessionFactory {
    pub(crate) fn new_server(session_factory: Rc<dyn TerminalSessionFactory>) -> Self {
        Self {
            session_factory: Some(session_factory),
            pending_title: None,
        }
    }

    /// An optimistic Pane owns no Terminal Session or server identity yet.
    pub(crate) fn pending() -> Self {
        Self {
            session_factory: None,
            pending_title: None,
        }
    }

    /// A disconnected server's Pane presents its retained title without a Terminal Session.
    pub(crate) fn unavailable(title: Option<String>) -> Self {
        Self {
            pending_title: title,
            ..Self::pending()
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
            .start(geometry, initial_appearance)
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
}
