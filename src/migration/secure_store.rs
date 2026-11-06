//! A [`Store`] over the desktop's own [`SecureFilesystem`], for a root whose files have fixed
//! names directly in one directory.
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use spaceterm_core::migration::{Area, Store, StoreError};

use crate::platform::secure_filesystem::{
    PrivateFileSnapshot, SecureCommitOutcome, SecureDirectory, SecureFilesystem,
    SecureFilesystemError,
};

// Archive oversized Settings without granting them admission to the ordinary reader.
const MAX_BYTES: usize = 32 * 1024 * 1024;
const ATTEMPTS: usize = 16;

pub(crate) struct SecureStore {
    filesystem: Arc<dyn SecureFilesystem>,
    home: PathBuf,
    /// Every file name the root holds, besides the runner's own.
    names: &'static [&'static str],
}

impl SecureStore {
    pub(crate) fn new(
        filesystem: Arc<dyn SecureFilesystem>,
        home: PathBuf,
        names: &'static [&'static str],
    ) -> Self {
        Self {
            filesystem,
            home,
            names,
        }
    }

    fn locate(&self, area: Area, path: &str) -> Result<(PathBuf, String), StoreError> {
        let mut directory = match area {
            Area::Root => self.home.clone(),
            Area::Backup => self.home.join("migration-backup"),
            Area::Reset => self.home.join("reset"),
        };
        let mut parts: Vec<&str> = path.split('/').collect();
        let name = parts.pop().ok_or(StoreError::Unsafe)?;
        if parts
            .iter()
            .chain([&name])
            .any(|part| part.is_empty() || *part == "." || *part == "..")
        {
            return Err(StoreError::Unsafe);
        }
        directory.extend(parts);
        Ok((directory, name.to_owned()))
    }

    fn snapshot(
        &self,
        directory: &SecureDirectory,
        name: &str,
    ) -> Result<Option<PrivateFileSnapshot>, StoreError> {
        let snapshot = self
            .filesystem
            .read_private_file(directory, OsStr::new(name), MAX_BYTES)
            .map_err(classify)?;
        if snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.bytes.len() > MAX_BYTES)
        {
            return Err(StoreError::Unsafe);
        }
        Ok(snapshot)
    }

    fn open(&self, directory: &Path) -> Result<Option<SecureDirectory>, StoreError> {
        self.filesystem
            .open_private_directory(directory)
            .map_err(classify)
    }
}

impl Store for SecureStore {
    fn list(&self, area: Area) -> Result<Vec<String>, StoreError> {
        let mut present = Vec::new();
        let (directory, _) = self.locate(area, "generation.json")?;
        let Some(directory) = self.open(&directory)? else {
            return Ok(present);
        };
        for name in self.names.iter().copied().chain(["generation.json"]) {
            // Presence alone, so an oversized file is still listed for its reader to report.
            if self
                .filesystem
                .read_private_file(&directory, OsStr::new(name), 0)
                .map_err(classify)?
                .is_some()
            {
                present.push(name.to_owned());
            }
        }
        Ok(present)
    }

    fn read(&self, area: Area, path: &str) -> Result<Option<Vec<u8>>, StoreError> {
        let (directory, name) = self.locate(area, path)?;
        let Some(directory) = self.open(&directory)? else {
            return Ok(None);
        };
        Ok(self
            .snapshot(&directory, &name)?
            .map(|snapshot| snapshot.bytes))
    }

    fn write(&self, area: Area, path: &str, bytes: &[u8]) -> Result<(), StoreError> {
        let (directory, name) = self.locate(area, path)?;
        let directory = self
            .filesystem
            .ensure_private_directory(&directory)
            .map_err(classify)?;
        for _ in 0..ATTEMPTS {
            let snapshot = self
                .filesystem
                .read_private_file(&directory, OsStr::new(&name), MAX_BYTES)
                .map_err(classify)?;
            if area == Area::Reset
                && path.starts_with("gen-")
                && let Some(snapshot) = &snapshot
            {
                return if snapshot.bytes == bytes {
                    Ok(())
                } else {
                    Err(StoreError::Conflict)
                };
            }
            let expected = snapshot.map(|snapshot| snapshot.identity);
            let mut nonce = [0; 16];
            getrandom::fill(&mut nonce).map_err(|_| StoreError::Unavailable)?;
            let prepared = match self.filesystem.prepare_private_file(
                &directory,
                OsStr::new(&name),
                bytes,
                nonce,
            ) {
                Ok(prepared) => prepared,
                Err(SecureFilesystemError::AlreadyExists) => continue,
                Err(error) => return Err(classify(error)),
            };
            let commit = self
                .filesystem
                .commit_private_file(prepared, expected.as_ref())
                .map_err(classify)?;
            return match commit.outcome {
                SecureCommitOutcome::Committed => Ok(()),
                SecureCommitOutcome::Conflict => Err(StoreError::Conflict),
                SecureCommitOutcome::CommittedButUnsynced => Err(StoreError::Unavailable),
            };
        }
        Err(StoreError::Unavailable)
    }

    fn archive_and_remove(
        &self,
        path: &str,
        save: &mut dyn FnMut(&[u8]) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        let (directory, name) = self.locate(Area::Root, path)?;
        let Some(directory) = self.open(&directory)? else {
            return Ok(());
        };
        let Some(snapshot) = self.snapshot(&directory, &name)? else {
            return Ok(());
        };
        save(&snapshot.bytes)?;
        self.filesystem
            .remove_private_file(&directory, OsStr::new(&name), &snapshot.identity)
            .map_err(classify)
    }

    fn remove(&self, area: Area, path: &str) -> Result<(), StoreError> {
        let (directory, name) = self.locate(area, path)?;
        let Some(directory) = self.open(&directory)? else {
            return Ok(());
        };
        let Some(snapshot) = self
            .filesystem
            .read_private_file(&directory, OsStr::new(&name), MAX_BYTES)
            .map_err(classify)?
        else {
            return Ok(());
        };
        self.filesystem
            .remove_private_file(&directory, OsStr::new(&name), &snapshot.identity)
            .map_err(classify)
    }
}

fn classify(error: SecureFilesystemError) -> StoreError {
    match error {
        SecureFilesystemError::Unsafe => StoreError::Unsafe,
        SecureFilesystemError::Conflict => StoreError::Conflict,
        _ => StoreError::Unavailable,
    }
}
