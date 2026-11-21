//! Shows a file selected in a Finder window.
use std::path::Path;

use objc2::rc::autoreleasepool;
use objc2_app_kit::NSWorkspace;
use objc2_foundation::NSString;

use crate::file_viewer::{FileViewer, Reveal, RevealUnavailable};

pub(crate) struct FinderViewer;

impl FileViewer for FinderViewer {
    /// Asks Finder from a thread of its own, as Zed's `reveal_path` does. On the UI thread,
    /// NSWorkspace waits for Finder's reply in a nested event loop, and that loop runs queued UI
    /// work while the caller still holds the UI state, which aborts SpaceTerm.
    fn reveal(&self, path: &Path) -> Reveal {
        let Some(path) = path.to_str().map(str::to_owned) else {
            return Box::pin(std::future::ready(Err(RevealUnavailable)));
        };
        let (answer, settled) = async_channel::bounded(1);
        let spawned = std::thread::Builder::new()
            .name("finder-reveal".to_owned())
            .spawn(move || {
                let _ = answer.try_send(select(&path));
            });
        if spawned.is_err() {
            return Box::pin(std::future::ready(Err(RevealUnavailable)));
        }
        Box::pin(async move { settled.recv().await.unwrap_or(Err(RevealUnavailable)) })
    }
}

fn select(path: &str) -> Result<(), RevealUnavailable> {
    autoreleasepool(|_| {
        // An empty root opens a new Finder window on the file's folder.
        NSWorkspace::sharedWorkspace()
            .selectFile_inFileViewerRootedAtPath(Some(&NSString::from_str(path)), &NSString::new())
            .then_some(())
            .ok_or(RevealUnavailable)
    })
}
