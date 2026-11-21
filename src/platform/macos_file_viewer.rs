//! Shows a file selected in a Finder window.
use std::path::Path;

use objc2::MainThreadMarker;
use objc2_app_kit::NSWorkspace;
use objc2_foundation::NSString;

use crate::file_viewer::{FileViewer, Reveal, RevealUnavailable};

pub(crate) struct FinderViewer;

impl FileViewer for FinderViewer {
    /// NSWorkspace answers at once, so the reveal is already settled.
    fn reveal(&self, path: &Path) -> Reveal {
        Box::pin(std::future::ready(select(path)))
    }
}

fn select(path: &Path) -> Result<(), RevealUnavailable> {
    if MainThreadMarker::new().is_none() {
        return Err(RevealUnavailable);
    }
    let path = path.to_str().ok_or(RevealUnavailable)?;
    // An empty root opens a new Finder window on the file's folder.
    NSWorkspace::sharedWorkspace()
        .selectFile_inFileViewerRootedAtPath(Some(&NSString::from_str(path)), &NSString::new())
        .then_some(())
        .ok_or(RevealUnavailable)
}
