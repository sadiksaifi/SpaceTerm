//! The only local-authority gate for Native Terminal Services. An admitted operation retains
//! this private proof instead of repeatedly interpreting metadata in each caller.
use super::clipboard::ClipboardError;
use super::file_insertion::{FileInsertionPolicy, prepare_file_insertion};
use crate::terminal::TerminalLocalFileCapabilities;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct LocalFileAccess(());

impl LocalFileAccess {
    pub(super) fn authorize(capabilities: TerminalLocalFileCapabilities) -> Option<Self> {
        capabilities.are_enabled().then_some(Self(()))
    }

    pub(super) async fn clipboard<Read>(
        self,
        clipboard: impl FnOnce() -> Read,
    ) -> Result<Vec<PathBuf>, ClipboardError>
    where
        Read: Future<Output = Result<Vec<PathBuf>, ClipboardError>>,
    {
        clipboard().await
    }

    pub(super) fn insertion(
        self,
        policy: FileInsertionPolicy,
        paths: &[PathBuf],
    ) -> Result<String, &'static str> {
        prepare_file_insertion(policy, paths).map(|insertion| insertion.text)
    }

    pub(super) fn revalidate(self, target: &super::hyperlink::HyperlinkTarget) -> Option<PathBuf> {
        target.revalidated_local_path(TerminalLocalFileCapabilities::Enabled)
    }
}
