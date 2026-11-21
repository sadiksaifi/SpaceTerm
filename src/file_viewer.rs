//! The desktop's file manager, which shows a file selected in its folder.
use std::future::Future;
use std::path::Path;
use std::pin::Pin;

/// The file manager is missing, refused or didn't answer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RevealUnavailable;

/// The file manager's answer to a reveal.
pub(crate) type Reveal = Pin<Box<dyn Future<Output = Result<(), RevealUnavailable>> + Send>>;

pub(crate) trait FileViewer: Send + Sync {
    /// Asks the file manager to show `path` selected in its folder. Called on the UI thread,
    /// which it never blocks and on which it runs no other work before it returns; the returned
    /// future settles with the file manager's answer.
    fn reveal(&self, path: &Path) -> Reveal;
}
