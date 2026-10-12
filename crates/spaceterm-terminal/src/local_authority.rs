use crate::hyperlink::{HyperlinkKind, HyperlinkTarget, parse_local_file_uri, stable_identity};
use crate::local_filesystem::{LocalFileEmissionRegistry, LocalFilesystemAuthority};
use crate::metadata::TerminalLocalFileCapabilities;
use std::path::{Path, PathBuf};
pub struct LocalFileAccess(());
impl LocalFileAccess {
    pub fn authorize(capabilities: TerminalLocalFileCapabilities) -> Option<Self> {
        capabilities.are_enabled().then_some(Self(()))
    }
    pub fn resolve(
        self,
        value: &str,
        directory: &Path,
        hostname: Option<&str>,
        filesystem: &LocalFilesystemAuthority,
    ) -> Option<HyperlinkTarget> {
        let path = parse_local_file_uri(filesystem.path_semantics(), value, hostname)?;
        HyperlinkTarget::from_local_file(filesystem.local_file(&path, directory)?)
    }

    pub fn emit(
        self,
        target: &HyperlinkTarget,
        registry: &mut LocalFileEmissionRegistry,
    ) -> Option<Vec<u8>> {
        (target.kind == HyperlinkKind::LocalPath).then_some(())?;
        registry.emit(target.local_file.as_ref()?)
    }

    pub fn restore(
        self,
        metadata: &[u8],
        registry: &LocalFileEmissionRegistry,
    ) -> Option<HyperlinkTarget> {
        HyperlinkTarget::from_local_file(registry.restore(metadata)?)
    }

    pub fn revalidate(self, target: &HyperlinkTarget) -> Option<PathBuf> {
        (target.kind == HyperlinkKind::LocalPath).then_some(())?;
        let file = target.local_file.as_ref()?;
        (file.canonical_path().to_str()? == target.value
            && stable_identity(target.kind, target.value.as_bytes()) == target.identity)
            .then_some(())?;
        file.revalidated_path()
    }
}
