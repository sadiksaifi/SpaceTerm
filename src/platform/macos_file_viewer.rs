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
        match path.to_str() {
            Some(path) => on_own_thread(path.to_owned(), select),
            None => Box::pin(std::future::ready(Err(RevealUnavailable))),
        }
    }
}

/// Runs `ask` with `path` on a new thread, and settles with its answer.
fn on_own_thread(path: String, ask: fn(&str) -> Result<(), RevealUnavailable>) -> Reveal {
    let (answer, settled) = async_channel::bounded(1);
    let spawned = std::thread::Builder::new()
        .name("finder-reveal".to_owned())
        .spawn(move || {
            let _ = answer.try_send(ask(&path));
        });
    if spawned.is_err() {
        return Box::pin(std::future::ready(Err(RevealUnavailable)));
    }
    Box::pin(async move { settled.recv().await.unwrap_or(Err(RevealUnavailable)) })
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::thread::ThreadId;

    static ASKED: Mutex<Option<(ThreadId, String)>> = Mutex::new(None);

    fn record(path: &str) -> Result<(), RevealUnavailable> {
        *ASKED.lock().unwrap() = Some((std::thread::current().id(), path.to_owned()));
        Err(RevealUnavailable)
    }

    #[test]
    fn finder_is_asked_from_another_thread_and_its_answer_settles_the_reveal() {
        let answer = futures_lite::future::block_on(on_own_thread("/tmp/launcher".into(), record));
        assert_eq!(answer, Err(RevealUnavailable));
        let (thread, path) = ASKED.lock().unwrap().clone().expect("Finder was asked");
        assert_ne!(thread, std::thread::current().id());
        assert_eq!(path, "/tmp/launcher");
    }
}
