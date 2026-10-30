//! Shows a file selected in a Finder window.
use std::path::Path;

use objc2::MainThreadMarker;
use objc2_app_kit::NSWorkspace;
use objc2_foundation::NSString;

use crate::command_line_tool::{FileViewer, ToolFailure};

pub(crate) struct FinderViewer;

impl FileViewer for FinderViewer {
    fn reveal(&self, path: &Path) -> Result<(), ToolFailure> {
        if MainThreadMarker::new().is_none() {
            return Err(ToolFailure::RevealUnavailable);
        }
        let path = path.to_str().ok_or(ToolFailure::RevealUnavailable)?;
        // An empty root opens a new Finder window on the file's folder.
        NSWorkspace::sharedWorkspace()
            .selectFile_inFileViewerRootedAtPath(Some(&NSString::from_str(path)), &NSString::new())
            .then_some(())
            .ok_or(ToolFailure::RevealUnavailable)
    }
}
