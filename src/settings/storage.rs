//! Identity-aware storage for the single appearance preferences and catalog document.

use std::sync::{Arc, Mutex};

use crate::platform::app_directories::AppDirectoryRoot;
use crate::platform::app_paths::{AppPaths, AppPathsError};
use crate::platform::secure_filesystem::{
    PrivateFileSnapshot, SecureCommitOutcome, SecureDirectory, SecureEntryIdentity,
    SecureFilesystemError,
};

pub(super) const MAXIMUM_DOCUMENT_BYTES: usize = 4 * 1024 * 1024;
const PREPARE_ATTEMPTS: usize = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum StorageError {
    #[error("settings storage is unavailable")]
    Unavailable,
    #[error("settings storage is unsafe")]
    Unsafe,
    #[error("settings storage changed; reload before saving")]
    Conflict,
    #[error("settings document exceeds its size limit")]
    TooLarge,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Durability {
    Synchronized,
    Uncertain,
}

/// A successful publication remains committed even if its identity cannot be refreshed.
pub(crate) struct StorageCommit {
    pub(crate) durability: Durability,
    pub(crate) identity: Option<SecureEntryIdentity>,
}

/// Supplies file effects without exposing paths or native handles to settings policy.
pub(crate) trait SettingsStorage: Send + Sync {
    fn read(&self) -> Result<Option<PrivateFileSnapshot>, StorageError>;
    fn write(
        &self,
        bytes: &[u8],
        expected: Option<&SecureEntryIdentity>,
    ) -> Result<StorageCommit, StorageError>;
}

pub(crate) struct ConfigSettingsStorage {
    paths: Arc<AppPaths>,
    directory: Mutex<Option<SecureDirectory>>,
}

impl ConfigSettingsStorage {
    pub(crate) fn new(paths: Arc<AppPaths>) -> Self {
        Self {
            paths,
            directory: Mutex::new(None),
        }
    }

    fn directory(&self, create: bool) -> Result<Option<SecureDirectory>, StorageError> {
        let mut retained = self
            .directory
            .lock()
            .map_err(|_| StorageError::Unavailable)?;
        if let Some(directory) = retained.as_ref() {
            self.paths
                .filesystem()
                .verify_directory(directory)
                .map_err(filesystem_error)?;
            return Ok(Some(directory.clone()));
        }
        let directory = if create {
            Some(
                self.paths
                    .ensure_secure_root(AppDirectoryRoot::Config)
                    .map_err(path_error)?,
            )
        } else {
            self.paths
                .open_secure_root(AppDirectoryRoot::Config)
                .map_err(path_error)?
        };
        *retained = directory.clone();
        Ok(directory)
    }
}

impl SettingsStorage for ConfigSettingsStorage {
    fn read(&self) -> Result<Option<PrivateFileSnapshot>, StorageError> {
        let Some(directory) = self.directory(false)? else {
            return Ok(None);
        };
        let target = self.paths.directories().config_file();
        let name = target.file_name().ok_or(StorageError::Unavailable)?;
        self.paths
            .filesystem()
            .read_private_file(&directory, name, MAXIMUM_DOCUMENT_BYTES)
            .map_err(filesystem_error)
    }

    fn write(
        &self,
        bytes: &[u8],
        expected: Option<&SecureEntryIdentity>,
    ) -> Result<StorageCommit, StorageError> {
        if bytes.len() > MAXIMUM_DOCUMENT_BYTES {
            return Err(StorageError::TooLarge);
        }
        let directory = self.directory(true)?.ok_or(StorageError::Unavailable)?;
        let filesystem = self.paths.filesystem();
        let target = self.paths.directories().config_file();
        let name = target.file_name().ok_or(StorageError::Unavailable)?;
        for _ in 0..PREPARE_ATTEMPTS {
            let mut nonce = [0; 16];
            getrandom::fill(&mut nonce).map_err(|_| StorageError::Unavailable)?;
            let prepared = match filesystem.prepare_private_file(&directory, name, bytes, nonce) {
                Ok(prepared) => prepared,
                Err(SecureFilesystemError::AlreadyExists) => continue,
                Err(error) => return Err(filesystem_error(error)),
            };
            let commit = filesystem
                .commit_private_file(prepared, expected)
                .map_err(filesystem_error)?;
            let durability = match commit.outcome {
                SecureCommitOutcome::Conflict => return Err(StorageError::Conflict),
                SecureCommitOutcome::Committed => Durability::Synchronized,
                SecureCommitOutcome::CommittedButUnsynced => Durability::Uncertain,
            };
            let identity = commit.published_identity.and_then(|published| {
                filesystem
                    .read_private_file(&directory, name, MAXIMUM_DOCUMENT_BYTES)
                    .ok()
                    .flatten()
                    .filter(|snapshot| snapshot.identity == published && snapshot.bytes == bytes)
                    .map(|snapshot| snapshot.identity)
            });
            return Ok(StorageCommit {
                durability,
                identity,
            });
        }
        Err(StorageError::Unavailable)
    }
}

fn path_error(error: AppPathsError) -> StorageError {
    match error {
        AppPathsError::UnsafePath | AppPathsError::InvalidArtifactName => StorageError::Unsafe,
        _ => StorageError::Unavailable,
    }
}

fn filesystem_error(error: SecureFilesystemError) -> StorageError {
    match error {
        SecureFilesystemError::Unsafe => StorageError::Unsafe,
        SecureFilesystemError::Missing
        | SecureFilesystemError::AlreadyExists
        | SecureFilesystemError::Unavailable => StorageError::Unavailable,
    }
}
