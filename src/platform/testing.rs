//! Deterministic capability implementations for portable contract tests.
use super::secure_filesystem::*;
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
#[derive(Default)]
pub(crate) struct RecordingFilesystem {
    directories: Mutex<BTreeSet<PathBuf>>,
    pub(crate) events: Mutex<Vec<&'static str>>,
    sockets: Mutex<std::collections::BTreeMap<PathBuf, u64>>,
    next: std::sync::atomic::AtomicU64,
    pub(crate) root_failure: Mutex<Option<SecureFilesystemError>>,
    prepared_files: Arc<AtomicUsize>,
    pub(crate) files: Mutex<RecordingPrivateFiles>,
}

#[derive(Default)]
pub(crate) struct RecordingPrivateFiles {
    pub(crate) values: std::collections::BTreeMap<PathBuf, (Vec<u8>, u64)>,
    pub(crate) successor_after_commit: Option<Vec<u8>>,
    pub(crate) commit_outcome: Option<SecureCommitOutcome>,
    pub(crate) prepare_failures: usize,
    pub(crate) prepare_error: Option<SecureFilesystemError>,
    pub(crate) prepare_count: usize,
    pub(crate) commit_error: Option<SecureFilesystemError>,
    pub(crate) read_failure: Option<SecureFilesystemError>,
}
#[derive(Clone)]
struct RecordingDirectory(PathBuf);

struct RecordingPreparedFile {
    path: PathBuf,
    bytes: Vec<u8>,
    _lease: RecordingPreparedFileLease,
}

struct RecordingPreparedFileLease(Arc<AtomicUsize>);

impl Drop for RecordingPreparedFileLease {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

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

    pub(crate) fn prepared_file_count(&self) -> usize {
        self.prepared_files.load(Ordering::Relaxed)
    }
}

impl SecureFilesystem for RecordingFilesystem {
    fn open_private_directory(
        &self,
        path: &Path,
    ) -> Result<Option<SecureDirectory>, SecureFilesystemError> {
        if let Some(error) = *self.root_failure.lock().unwrap() {
            return Err(error);
        }
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
        if let Some(error) = *self.root_failure.lock().unwrap() {
            return Err(error);
        }
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
        if let Some(error) = *self.root_failure.lock().unwrap() {
            return Err(error);
        }
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
        directory: &SecureDirectory,
        name: &OsStr,
        maximum_bytes: usize,
    ) -> Result<Option<PrivateFileSnapshot>, SecureFilesystemError> {
        let files = self.files.lock().unwrap();
        if let Some(error) = files.read_failure {
            return Err(error);
        }
        files
            .values
            .get(&Self::path(directory)?.join(name))
            .map(|(bytes, identity)| {
                if bytes.len() > maximum_bytes {
                    return Err(SecureFilesystemError::Unsafe);
                }
                Ok(PrivateFileSnapshot {
                    bytes: bytes.clone(),
                    identity: SecureEntryIdentity::from_opaque(*identity),
                })
            })
            .transpose()
    }
    fn prepare_private_file(
        &self,
        directory: &SecureDirectory,
        name: &OsStr,
        bytes: &[u8],
        _: [u8; 16],
    ) -> Result<PreparedPrivateFile, SecureFilesystemError> {
        let mut files = self.files.lock().unwrap();
        files.prepare_count += 1;
        if let Some(error) = files.prepare_error {
            return Err(error);
        }
        if files.prepare_failures > 0 {
            files.prepare_failures -= 1;
            return Err(SecureFilesystemError::AlreadyExists);
        }
        let path = Self::path(directory)?.join(name);
        self.prepared_files.fetch_add(1, Ordering::Relaxed);
        Ok(PreparedPrivateFile::from_opaque(RecordingPreparedFile {
            path,
            bytes: bytes.to_vec(),
            _lease: RecordingPreparedFileLease(Arc::clone(&self.prepared_files)),
        }))
    }
    fn commit_private_file(
        &self,
        prepared: PreparedPrivateFile,
        expected: Option<&SecureEntryIdentity>,
    ) -> Result<SecureCommitResult, SecureFilesystemError> {
        let RecordingPreparedFile {
            path,
            bytes,
            _lease,
        } = *prepared
            .into_opaque::<RecordingPreparedFile>()
            .map_err(|_| SecureFilesystemError::Unsafe)?;
        let expected = expected
            .and_then(|identity| identity.opaque_ref::<u64>())
            .copied();
        let mut files = self.files.lock().unwrap();
        if let Some(error) = files.commit_error {
            return Err(error);
        }
        if files.values.get(&path).map(|(_, identity)| *identity) != expected {
            return Ok(SecureCommitResult::conflict());
        }
        let outcome = files
            .commit_outcome
            .unwrap_or(SecureCommitOutcome::Committed);
        if outcome == SecureCommitOutcome::Conflict {
            return Ok(SecureCommitResult::conflict());
        }
        let identity = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        files.values.insert(path.clone(), (bytes, identity));
        if let Some(bytes) = files.successor_after_commit.take() {
            files.values.insert(path, (bytes, identity + 1));
        }
        Ok(SecureCommitResult::committed(
            outcome,
            SecureEntryIdentity::from_opaque(identity),
        ))
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
