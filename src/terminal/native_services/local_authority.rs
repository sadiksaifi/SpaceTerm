//! The only local-authority gate for Native Terminal Services. An admitted operation retains
//! this private proof instead of repeatedly interpreting metadata in each caller.
use super::clipboard::{ClipboardError, FileClipboard};
use super::file_insertion::{FileInsertionPolicy, prepare_file_insertion};
use super::hyperlink::{HyperlinkKind, HyperlinkTarget, parse_local_file_uri, stable_identity};
use crate::platform::local_filesystem::{LocalFileEmissionRegistry, LocalFilesystemAuthority};
use crate::terminal::TerminalLocalFileCapabilities;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct LocalFileAccess(());

impl LocalFileAccess {
    pub(super) fn authorize(capabilities: TerminalLocalFileCapabilities) -> Option<Self> {
        capabilities.are_enabled().then_some(Self(()))
    }

    pub(super) fn clipboard(
        self,
        clipboard: &dyn FileClipboard,
    ) -> Result<Vec<PathBuf>, ClipboardError> {
        clipboard.read_files()
    }

    pub(super) fn insertion(
        self,
        policy: FileInsertionPolicy,
        paths: &[PathBuf],
    ) -> Result<String, &'static str> {
        prepare_file_insertion(policy, paths).map(|insertion| insertion.text)
    }

    pub(super) fn resolve(
        self,
        value: &str,
        directory: &Path,
        hostname: Option<&str>,
        filesystem: &LocalFilesystemAuthority,
    ) -> Option<HyperlinkTarget> {
        let path = parse_local_file_uri(filesystem.path_semantics(), value, hostname)?;
        HyperlinkTarget::from_local_file(filesystem.local_file(&path, directory)?)
    }

    pub(super) fn emit(
        self,
        target: &HyperlinkTarget,
        registry: &mut LocalFileEmissionRegistry,
    ) -> Option<Vec<u8>> {
        (target.kind == HyperlinkKind::LocalPath).then_some(())?;
        registry.emit(target.local_file.as_ref()?)
    }

    pub(super) fn restore(
        self,
        metadata: &[u8],
        registry: &LocalFileEmissionRegistry,
    ) -> Option<HyperlinkTarget> {
        HyperlinkTarget::from_local_file(registry.restore(metadata)?)
    }

    pub(super) fn revalidate(self, target: &HyperlinkTarget) -> Option<PathBuf> {
        (target.kind == HyperlinkKind::LocalPath).then_some(())?;
        let file = target.local_file.as_ref()?;
        (file.canonical_path().to_str()? == target.value
            && stable_identity(target.kind, target.value.as_bytes()) == target.identity)
            .then_some(())?;
        file.revalidated_path()
    }
}
