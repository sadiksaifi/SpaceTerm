//! Shows a file selected in the desktop's file manager through the FileManager1 interface,
//! which Files, Dolphin, Nemo, Caja and Thunar provide.
use std::path::Path;

use super::linux_session_bus::SessionBus;
use crate::command_line_tool::{FileViewer, ToolFailure};

const SERVICE: &str = "org.freedesktop.FileManager1";
const PATH: &str = "/org/freedesktop/FileManager1";

pub(crate) struct FileManagerViewer {
    bus: Option<SessionBus>,
}

impl FileManagerViewer {
    pub(crate) fn new(bus: Option<SessionBus>) -> Self {
        Self { bus }
    }
}

impl FileViewer for FileManagerViewer {
    /// Queues the request on the session bus. A file manager that never answers shows nothing.
    fn reveal(&self, path: &Path) -> Result<(), ToolFailure> {
        let bus = self.bus.as_ref().ok_or(ToolFailure::RevealUnavailable)?;
        let uri = super::linux_file_preview::file_uri(path);
        bus.dispatch(move |connection| {
            // GPUI exposes no activation token to pass on, and file managers accept an empty one.
            let _ = connection.call_method(
                Some(SERVICE),
                PATH,
                Some(SERVICE),
                "ShowItems",
                &(vec![uri.as_str()], ""),
            );
        })
        .map_err(|_| ToolFailure::RevealUnavailable)
    }
}
