//! Deterministic capability implementations for portable contract tests.
use super::secure_filesystem::*;
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
#[derive(Default)]
pub(crate) struct RecordingFilesystem {
    directories: Mutex<BTreeSet<PathBuf>>,
    pub(crate) events: Mutex<Vec<&'static str>>,
    sockets: Mutex<std::collections::BTreeMap<PathBuf, u64>>,
    next: std::sync::atomic::AtomicU64,
}
#[derive(Clone)]
struct RecordingDirectory(PathBuf);

impl RecordingFilesystem {
    fn path(directory: &SecureDirectory) -> Result<&PathBuf, SecureFilesystemError> {
        directory
            .0
            .downcast_ref::<RecordingDirectory>()
            .map(|directory| &directory.0)
            .ok_or(SecureFilesystemError::Unsafe)
    }
    fn directory(path: PathBuf) -> SecureDirectory {
        SecureDirectory(Arc::new(RecordingDirectory(path)))
    }
}

impl SecureFilesystem for RecordingFilesystem {
    fn open_private_directory(
        &self,
        path: &Path,
    ) -> Result<Option<SecureDirectory>, SecureFilesystemError> {
        Ok(self
            .directories
            .lock()
            .unwrap()
            .contains(path)
            .then(|| Self::directory(path.to_path_buf())))
    }
    fn ensure_private_directory(
        &self,
        path: &Path,
    ) -> Result<SecureDirectory, SecureFilesystemError> {
        self.events.lock().unwrap().push("ensure");
        self.directories.lock().unwrap().insert(path.to_path_buf());
        Ok(Self::directory(path.to_path_buf()))
    }
    fn create_private_child(
        &self,
        parent: &SecureDirectory,
        name: &OsStr,
    ) -> Result<SecureDirectory, SecureFilesystemError> {
        self.events.lock().unwrap().push("create-owner");
        let path = Self::path(parent)?.join(name);
        if !self.directories.lock().unwrap().insert(path.clone()) {
            return Err(SecureFilesystemError::AlreadyExists);
        }
        Ok(Self::directory(path))
    }
    fn verify_directory(&self, _: &SecureDirectory) -> Result<(), SecureFilesystemError> {
        self.events.lock().unwrap().push("verify");
        Ok(())
    }
    fn remove_private_child(
        &self,
        _: &SecureDirectory,
        _: &OsStr,
        _: &SecureDirectory,
    ) -> Result<(), SecureFilesystemError> {
        self.events.lock().unwrap().push("remove-owner");
        Ok(())
    }
    fn read_private_file(
        &self,
        _: &SecureDirectory,
        _: &OsStr,
        _: usize,
    ) -> Result<Option<PrivateFileSnapshot>, SecureFilesystemError> {
        Ok(None)
    }
    fn prepare_private_file(
        &self,
        _: &SecureDirectory,
        _: &OsStr,
        _: &[u8],
        _: [u8; 16],
    ) -> Result<PreparedPrivateFile, SecureFilesystemError> {
        Ok(PreparedPrivateFile(Box::new(())))
    }
    fn commit_private_file(
        &self,
        _: PreparedPrivateFile,
        _: Option<&SecureEntryIdentity>,
    ) -> Result<SecureCommitOutcome, SecureFilesystemError> {
        Ok(SecureCommitOutcome::Committed)
    }
    fn register_socket(
        &self,
        directory: &SecureDirectory,
        name: &OsStr,
    ) -> Result<SecureEntryIdentity, SecureFilesystemError> {
        self.events.lock().unwrap().push("register");
        let path = Self::path(directory)?.join(name);
        let id = self
            .sockets
            .lock()
            .unwrap()
            .get(&path)
            .copied()
            .ok_or(SecureFilesystemError::Missing)?;
        Ok(SecureEntryIdentity::from_opaque(id))
    }
    fn verify_socket(
        &self,
        directory: &SecureDirectory,
        name: &OsStr,
        identity: &SecureEntryIdentity,
    ) -> Result<(), SecureFilesystemError> {
        self.events.lock().unwrap().push("verify-socket");
        let path = Self::path(directory)?.join(name);
        if identity
            .opaque_ref::<u64>()
            .is_some_and(|expected| self.sockets.lock().unwrap().get(&path) == Some(expected))
        {
            Ok(())
        } else {
            Err(SecureFilesystemError::Unsafe)
        }
    }
    fn remove_socket(
        &self,
        directory: &SecureDirectory,
        name: &OsStr,
        identity: &SecureEntryIdentity,
    ) -> Result<(), SecureFilesystemError> {
        self.verify_socket(directory, name, identity)?;
        self.events.lock().unwrap().push("remove-socket");
        self.sockets
            .lock()
            .unwrap()
            .remove(&Self::path(directory)?.join(name));
        Ok(())
    }
    #[cfg(feature = "macos-native-tests")]
    fn create_private_artifact(
        &self,
        _: &SecureDirectory,
        _: &OsStr,
    ) -> Result<(), SecureFilesystemError> {
        Ok(())
    }
}

impl RecordingFilesystem {
    pub(crate) fn create_socket(&self, path: &Path) {
        let id = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.sockets.lock().unwrap().insert(path.to_owned(), id);
    }
    pub(crate) fn has_socket(&self, path: &Path) -> bool {
        self.sockets.lock().unwrap().contains_key(path)
    }
    pub(crate) fn delete_socket(&self, path: &Path) {
        self.sockets.lock().unwrap().remove(path);
    }
}
#[derive(Default)]
pub(crate) struct RecordingControlSocketProbe(pub(crate) Arc<RecordingFilesystem>);
impl super::control_socket::ControlSocketProbe for RecordingControlSocketProbe {
    fn probe(
        &self,
        endpoint: &Path,
    ) -> Result<(), super::control_socket::ControlSocketUnavailable> {
        self.0.create_socket(endpoint);
        Ok(())
    }
}
pub(crate) struct EmptyHostConfigFilesystem;
impl crate::ssh::host_config::HostConfigFilesystem for EmptyHostConfigFilesystem {
    fn canonicalize(
        &self,
        _: &Path,
    ) -> Result<PathBuf, crate::ssh::host_config::HostConfigFilesystemError> {
        Err(crate::ssh::host_config::HostConfigFilesystemError::Missing)
    }
    fn read_file_limited(
        &self,
        _: &Path,
        _: usize,
    ) -> Result<Vec<u8>, crate::ssh::host_config::HostConfigFilesystemError> {
        Err(crate::ssh::host_config::HostConfigFilesystemError::Missing)
    }
    fn read_directory_limited(
        &self,
        _: &Path,
        _: usize,
    ) -> Result<Vec<PathBuf>, crate::ssh::host_config::HostConfigFilesystemError> {
        Err(crate::ssh::host_config::HostConfigFilesystemError::Missing)
    }
}
