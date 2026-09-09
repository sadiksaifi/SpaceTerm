//! Portable Local Filesystem Authority. Native mechanics expose only retained object identity.
use std::collections::VecDeque;
use std::fmt;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use thiserror::Error;

use crate::domain::ValidatedLocalDirectory;

pub(crate) mod picker;

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(crate) enum LocalFilesystemError {
    #[error("the path is not absolute")]
    NotAbsolute,
    #[error("the path is malformed")]
    Malformed,
    #[error("permission denied")]
    PermissionDenied,
    #[error("the path is missing")]
    Missing,
    #[error("the path is not a directory")]
    NotDirectory,
    #[error("the path is not a regular file")]
    NotFile,
    #[error("the directory is not readable")]
    Unreadable,
    #[error("the path no longer identifies the selected object")]
    IdentityChanged,
    #[error("local filesystem capacity is temporarily exhausted")]
    Capacity,
    #[error("the filesystem operation failed")]
    Other,
}

pub(super) fn classify_io_error(error: io::Error) -> LocalFilesystemError {
    match error.kind() {
        io::ErrorKind::PermissionDenied => LocalFilesystemError::PermissionDenied,
        io::ErrorKind::NotFound => LocalFilesystemError::Missing,
        io::ErrorKind::NotADirectory => LocalFilesystemError::NotDirectory,
        io::ErrorKind::InvalidInput | io::ErrorKind::InvalidFilename => {
            LocalFilesystemError::Malformed
        }
        _ => LocalFilesystemError::Other,
    }
}

#[derive(Clone, Eq, Hash, PartialEq)]
enum IdentityValue {
    Retained(Arc<same_file::Handle>),
    Unavailable,
    #[cfg(test)]
    Fixture(u64),
    #[cfg(test)]
    FixturePath(PathBuf),
}

/// Equality retains the object, so removal cannot recycle its identity into a successor.
#[derive(Clone)]
pub(crate) struct LocalObjectIdentity(IdentityValue, Option<Arc<IdentityPermit>>);

impl PartialEq for LocalObjectIdentity {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}
impl Eq for LocalObjectIdentity {}

impl Hash for LocalObjectIdentity {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

// Bound all open identity handles, including transient observations, across the application.
// File targets have a smaller allowance so output cannot consume directory validation capacity.
const MAX_IDENTITY_HANDLES: usize = 96;
const MAX_LOCAL_FILE_LEASES: usize = 64;

#[derive(Default)]
struct IdentityBudget(AtomicUsize);

impl IdentityBudget {
    fn reserve(self: &Arc<Self>, limit: usize) -> Result<IdentityPermit, LocalFilesystemError> {
        self.0
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                (used < limit).then_some(used + 1)
            })
            .map(|_| IdentityPermit(Arc::clone(self)))
            .map_err(|_| LocalFilesystemError::Capacity)
    }
}

struct IdentityPermit(Arc<IdentityBudget>);

impl Drop for IdentityPermit {
    fn drop(&mut self) {
        self.0.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl fmt::Debug for LocalObjectIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LocalObjectIdentity(<redacted>)")
    }
}

impl LocalObjectIdentity {
    pub(super) fn from_file(file: fs::File) -> Result<Self, LocalFilesystemError> {
        same_file::Handle::from_file(file)
            .map(|handle| Self(IdentityValue::Retained(Arc::new(handle)), None))
            .map_err(classify_io_error)
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct LocalDirectoryIdentity(LocalObjectIdentity);

impl LocalDirectoryIdentity {
    /// An unavailable startup directory carries no filesystem authority.
    pub(crate) fn unavailable() -> Self {
        Self(LocalObjectIdentity(IdentityValue::Unavailable, None))
    }

    #[cfg(test)]
    pub(crate) fn for_test(label: u64) -> Self {
        Self(LocalObjectIdentity(IdentityValue::Fixture(label), None))
    }

    #[cfg(test)]
    pub(crate) fn is_synthetic(&self) -> bool {
        matches!(self.0.0, IdentityValue::Fixture(0))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LocalObjectKind {
    Directory,
    File,
    Other,
}

pub(crate) struct LocalIdentityObservation {
    pub(crate) identity: LocalObjectIdentity,
    pub(crate) kind: LocalObjectKind,
}

/// The sole irreducible operation: capture the identity and type of one retained object.
/// Implementations must not read file contents or block opening a special file.
pub(crate) trait LocalIdentitySource: Send + Sync {
    fn identify(&self, path: &Path) -> Result<LocalIdentityObservation, LocalFilesystemError>;
}

#[derive(Clone)]
pub(crate) struct LocalFilesystemAuthority {
    paths: crate::local_path::LocalPathSemantics,
    identity: Arc<dyn LocalIdentitySource>,
    handles: Arc<IdentityBudget>,
    files: Arc<IdentityBudget>,
}

impl LocalFilesystemAuthority {
    pub(crate) fn new(
        paths: crate::local_path::LocalPathSemantics,
        identity: Arc<dyn LocalIdentitySource>,
    ) -> Self {
        Self {
            paths,
            identity,
            handles: Arc::default(),
            files: Arc::default(),
        }
    }

    pub(crate) fn path_semantics(&self) -> crate::local_path::LocalPathSemantics {
        self.paths
    }

    pub(crate) fn validate_directory(
        &self,
        path: &Path,
    ) -> Result<ValidatedLocalDirectory, LocalFilesystemError> {
        validate_absolute_path(self.paths, path)?;
        let first = self.identify_kind(path, LocalObjectKind::Directory)?;
        fs::read_dir(path).map_err(|error| match classify_io_error(error) {
            LocalFilesystemError::Other => LocalFilesystemError::Unreadable,
            error => error,
        })?;
        let current = self.identify_kind(path, LocalObjectKind::Directory)?;
        if first != current {
            return Err(LocalFilesystemError::IdentityChanged);
        }
        Ok(ValidatedLocalDirectory::new(
            path.to_owned(),
            LocalDirectoryIdentity(first),
        ))
    }

    pub(crate) fn revalidate_directory(
        &self,
        directory: &ValidatedLocalDirectory,
    ) -> Result<ValidatedLocalDirectory, LocalFilesystemError> {
        let current = self.validate_directory(directory.path())?;
        if current.identity() != directory.identity() {
            return Err(LocalFilesystemError::IdentityChanged);
        }
        Ok(current)
    }

    fn identify_kind(
        &self,
        path: &Path,
        kind: LocalObjectKind,
    ) -> Result<LocalObjectIdentity, LocalFilesystemError> {
        let permit = self.handles.reserve(MAX_IDENTITY_HANDLES)?;
        let mut observation = self.identity.identify(path)?;
        if observation.kind != kind {
            return Err(match kind {
                LocalObjectKind::Directory => LocalFilesystemError::NotDirectory,
                LocalObjectKind::File | LocalObjectKind::Other => LocalFilesystemError::NotFile,
            });
        }
        observation.identity.1 = Some(Arc::new(permit));
        Ok(observation.identity)
    }

    pub(crate) fn local_file(&self, value: &str, directory: &Path) -> Option<ValidatedLocalFile> {
        valid_file_text(value).then_some(())?;
        let path = Path::new(value);
        let selected = if self.paths.is_absolute(path) {
            path.to_owned()
        } else {
            validate_absolute_path(self.paths, directory).ok()?;
            directory.join(path)
        };
        validate_absolute_path(self.paths, &selected).ok()?;
        let permit = self.files.reserve(MAX_LOCAL_FILE_LEASES).ok()?;
        let identity = self.identify_kind(&selected, LocalObjectKind::File).ok()?;
        let canonical = fs::canonicalize(&selected).ok()?;
        valid_file_text(canonical.to_str()?).then_some(())?;
        let file = ValidatedLocalFile(Arc::new(LocalFileState {
            selected,
            canonical,
            identity,
            authority: self.clone(),
            _permit: permit,
        }));
        file.revalidated_path()?;
        Some(file)
    }
}

fn validate_absolute_path(
    semantics: crate::local_path::LocalPathSemantics,
    path: &Path,
) -> Result<(), LocalFilesystemError> {
    if !semantics.is_absolute(path) {
        return Err(LocalFilesystemError::NotAbsolute);
    }
    if path.as_os_str().as_encoded_bytes().contains(&0) {
        return Err(LocalFilesystemError::Malformed);
    }
    Ok(())
}

const MAX_LOCAL_FILE_PATH_BYTES: usize = 4096;

fn valid_file_text(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_LOCAL_FILE_PATH_BYTES
        && !value.chars().any(char::is_control)
}

struct LocalFileState {
    selected: PathBuf,
    canonical: PathBuf,
    identity: LocalObjectIdentity,
    authority: LocalFilesystemAuthority,
    _permit: IdentityPermit,
}

#[derive(Clone)]
pub(crate) struct ValidatedLocalFile(Arc<LocalFileState>);

impl fmt::Debug for ValidatedLocalFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ValidatedLocalFile(<redacted>)")
    }
}

impl PartialEq for ValidatedLocalFile {
    fn eq(&self, other: &Self) -> bool {
        self.0.selected == other.0.selected
            && self.0.canonical == other.0.canonical
            && self.0.identity == other.0.identity
    }
}
impl Eq for ValidatedLocalFile {}

impl ValidatedLocalFile {
    pub(crate) fn path_semantics(&self) -> crate::local_path::LocalPathSemantics {
        self.0.authority.paths
    }
    pub(crate) fn canonical_path(&self) -> &Path {
        &self.0.canonical
    }

    pub(crate) fn revalidated_path(&self) -> Option<PathBuf> {
        let state = &self.0;
        let selected = state
            .authority
            .identify_kind(&state.selected, LocalObjectKind::File)
            .ok()?;
        let canonical = fs::canonicalize(&state.selected).ok()?;
        if selected != state.identity || canonical != state.canonical {
            return None;
        }
        let current = state
            .authority
            .identify_kind(&canonical, LocalObjectKind::File)
            .ok()?;
        (current == state.identity).then_some(canonical)
    }
}

const EMISSION_PREFIX: &[u8; 8] = b"STLF\0\0\0\x02";
const MAX_EMITTED_LOCAL_FILES: usize = 32;

/// One Terminal Emulator owns this bounded lease table. Bytes reveal no paths or native identity.
/// Eviction revokes old metadata; tokens are never reused, even after a file is replaced.
#[derive(Default)]
pub(crate) struct LocalFileEmissionRegistry {
    files: VecDeque<([u8; 24], ValidatedLocalFile)>,
}

impl LocalFileEmissionRegistry {
    /// Release the oldest registry lease before resolution opens another identity handle.
    /// A snapshot may still retain it; the shared budget counts that lifetime independently.
    pub(crate) fn prepare_resolution(&mut self) {
        if self.files.len() == MAX_EMITTED_LOCAL_FILES {
            self.files.pop_front();
        }
    }

    pub(crate) fn emit(&mut self, file: &ValidatedLocalFile) -> Option<Vec<u8>> {
        if let Some((token, _)) = self.files.iter().find(|(_, retained)| retained == file) {
            return Some(token.to_vec());
        }
        let mut token = [0; 24];
        token[..8].copy_from_slice(EMISSION_PREFIX);
        getrandom::fill(&mut token[8..]).ok()?;
        if self.files.len() == MAX_EMITTED_LOCAL_FILES {
            self.files.pop_front();
        }
        self.files.push_back((token, file.clone()));
        Some(token.to_vec())
    }

    pub(crate) fn restore(&self, metadata: &[u8]) -> Option<ValidatedLocalFile> {
        if metadata.len() != 24 || !metadata.starts_with(EMISSION_PREFIX) {
            return None;
        }
        self.files
            .iter()
            .find(|(token, _)| token == metadata)
            .map(|(_, file)| file.clone())
    }
}

#[cfg(test)]
mod tests;

#[cfg(all(test, target_os = "macos", feature = "macos-native-tests"))]
#[path = "macos_adapter_tests/local_filesystem.rs"]
mod macos_adapter_tests;
