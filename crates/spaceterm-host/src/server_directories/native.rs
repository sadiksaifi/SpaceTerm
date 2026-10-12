//! Owning-filesystem authority. Paths remain host values, never client-local capabilities.

use super::browse::BrowseObservation;
use super::validation::CreateTargetLease;
use crate::git::filesystem::{
    DirectoryError, NativeWorktreeFilesystem, RetainedDirectory, WorktreeFilesystem,
};
use spaceterm_core::identity::ServerId;
use spaceterm_core::organization::ServerDirectory;
use spaceterm_core::server_directories::*;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Owning-server read provider with bounded private cursor and directory leases.
pub struct NativeDirectoryProvider {
    pub(crate) server: ServerId,
    pub(crate) home: ServerDirectory,
    pub(crate) filesystem: Arc<dyn WorktreeFilesystem>,
    pub(crate) state: Mutex<BrowseState>,
}
#[derive(Default)]
pub(crate) struct BrowseState {
    pub next_observation: u64,
    pub observations: BTreeMap<[u8; 32], BrowseObservation>,
}
impl NativeDirectoryProvider {
    /// Bind the provider to the account's validated server-side home.
    pub fn new(server: ServerId, home: PathBuf) -> Result<Self, DirectoryFailure> {
        let home = ServerDirectory::new(server, home.as_os_str().as_encoded_bytes().to_vec())
            .map_err(|_| DirectoryFailure::InvalidDirectory)?;
        let provider = Self {
            server,
            home,
            filesystem: Arc::new(NativeWorktreeFilesystem),
            state: Mutex::default(),
        };
        provider.retain(&provider.home)?;
        Ok(provider)
    }
    /// Retain native authority for launch, repair or confirmation revalidation.
    pub fn retain(
        &self,
        directory: &ServerDirectory,
    ) -> Result<Arc<dyn RetainedDirectory>, DirectoryFailure> {
        self.filesystem
            .validate_directory(&self.path(directory)?)
            .map_err(classify)
    }
    pub(crate) fn path(&self, directory: &ServerDirectory) -> Result<PathBuf, DirectoryFailure> {
        if directory.server() != self.server {
            return Err(DirectoryFailure::WrongServer);
        }
        let path = PathBuf::from(OsString::from_vec(directory.bytes().to_vec()));
        if !path.is_absolute()
            || path
                .components()
                .any(|p| !matches!(p, Component::RootDir | Component::Normal(_)))
        {
            return Err(DirectoryFailure::InvalidDirectory);
        }
        // Path::components normalizes dot segments. Reject them before that normalization.
        if directory
            .bytes()
            .split(|b| *b == b'/')
            .any(|p| p == b"." || p == b"..")
        {
            return Err(DirectoryFailure::InvalidDirectory);
        }
        Ok(path)
    }
    pub(crate) fn value(&self, path: &Path) -> Result<ServerDirectory, DirectoryFailure> {
        ServerDirectory::new(self.server, path.as_os_str().as_encoded_bytes().to_vec())
            .map_err(|_| DirectoryFailure::InvalidDirectory)
    }
}
impl ServerDirectoryQueries for NativeDirectoryProvider {
    fn query(&self, request: &DirectoryQuery) -> Result<DirectoryQueryResult, DirectoryFailure> {
        if request.parts().context.server() != self.server {
            return Err(DirectoryFailure::WrongServer);
        }
        match &request.parts().action {
            DirectoryQueryAction::Home => Ok(DirectoryQueryResult::Home(self.home.clone())),
            DirectoryQueryAction::Browse { directory, cursor } => self
                .browse(request.parts().context, directory, cursor.as_ref())
                .map(|p| DirectoryQueryResult::Page(Box::new(p))),
            DirectoryQueryAction::Validate { directory, purpose } => self
                .validate(directory, *purpose)
                .map(DirectoryQueryResult::Validation),
        }
    }
    fn containment(
        &self,
        ancestor: &ServerDirectory,
        candidate: &ServerDirectory,
    ) -> Result<DirectoryContainment, DirectoryFailure> {
        let candidate_path = self.path(candidate)?;
        let observe = || match self.retain(ancestor) {
            Ok(a) => {
                let c = self.filesystem.validate_directory(&candidate_path).ok()?;
                let physical_a = physical_path(&*a).ok()?;
                let physical_c = physical_path(&*c).ok()?;
                a.revalidate().ok()?;
                c.revalidate().ok()?;
                Some(classify_containment(&physical_a, &physical_c))
            }
            Err(DirectoryFailure::Missing) => {
                let a = self.retain_absent_removal_target(ancestor).ok()?;
                self.absent_containment(&a, candidate).ok()
            }
            Err(_) => None,
        };
        self.path(ancestor)?;
        Ok(observe().unwrap_or(DirectoryContainment::Unchecked))
    }
}
impl NativeDirectoryProvider {
    fn absent_containment(
        &self,
        ancestor: &CreateTargetLease,
        candidate: &ServerDirectory,
    ) -> Result<DirectoryContainment, DirectoryFailure> {
        let physical_a = ancestor.physical_path()?;
        let (physical_c, absent, existing) = match self.retain(candidate) {
            Ok(c) => (physical_path(&*c)?, None, Some(c)),
            Err(DirectoryFailure::Missing) => {
                let c = self.retain_absent_removal_target(candidate)?;
                (c.physical_path()?, Some(c), None)
            }
            Err(error) => return Err(error),
        };
        ancestor.revalidate()?;
        if let Some(c) = absent {
            c.revalidate()?;
        }
        if let Some(c) = existing {
            c.revalidate().map_err(classify)?;
        }
        Ok(classify_containment(&physical_a, &physical_c))
    }
}
fn classify_containment(ancestor: &Path, candidate: &Path) -> DirectoryContainment {
    if candidate.starts_with(ancestor) {
        DirectoryContainment::Inside
    } else {
        DirectoryContainment::Outside
    }
}
/// Check canonical spellings against retained identity without extra native leases.
pub(crate) fn physical_path(
    directory: &dyn RetainedDirectory,
) -> Result<PathBuf, DirectoryFailure> {
    use std::os::unix::fs::MetadataExt;
    let path = std::fs::canonicalize(directory.path()).map_err(io_failure)?;
    directory.revalidate().map_err(classify)?;
    let selected = std::fs::metadata(directory.path()).map_err(io_failure)?;
    let physical = std::fs::metadata(&path).map_err(io_failure)?;
    if (selected.dev(), selected.ino()) != (physical.dev(), physical.ino()) {
        return Err(DirectoryFailure::IdentityChanged);
    }
    directory.revalidate().map_err(classify)?;
    Ok(path)
}

pub(crate) fn classify(error: DirectoryError) -> DirectoryFailure {
    match error {
        DirectoryError::Missing => DirectoryFailure::Missing,
        DirectoryError::NotDirectory => DirectoryFailure::NotDirectory,
        DirectoryError::IdentityChanged => DirectoryFailure::IdentityChanged,
        DirectoryError::Capacity => DirectoryFailure::Capacity,
        DirectoryError::Unavailable => DirectoryFailure::Unavailable,
    }
}
pub(crate) fn io_failure(error: std::io::Error) -> DirectoryFailure {
    match error.kind() {
        std::io::ErrorKind::NotFound => DirectoryFailure::Missing,
        std::io::ErrorKind::NotADirectory => DirectoryFailure::NotDirectory,
        _ => DirectoryFailure::Unavailable,
    }
}
