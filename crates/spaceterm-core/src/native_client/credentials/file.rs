//! CLI-only private files. Directory descriptors fence every operation against path replacement.
use std::ffi::{CStr, CString};
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, PathBuf};

use super::{CredentialError, CredentialStore, Interaction, StoreKind, account};
use zeroize::Zeroizing;

pub(super) struct PrivateFiles(pub(super) PathBuf);

impl PrivateFiles {
    fn directory(&self, create: bool) -> Result<Option<File>, CredentialError> {
        if !self.0.is_absolute() {
            return Err(CredentialError::Unavailable);
        }
        let mut directory = File::open("/").map_err(unavailable)?;
        let components: Vec<_> = self.0.components().skip(1).collect();
        for (index, component) in components.iter().enumerate() {
            let Component::Normal(name) = component else {
                return Err(CredentialError::Unavailable);
            };
            let name = CString::new(name.as_bytes()).map_err(unavailable)?;
            if create {
                // SAFETY: the descriptor and terminated component remain live.
                let result = unsafe { libc::mkdirat(directory.as_raw_fd(), name.as_ptr(), 0o700) };
                if result == 0 {
                    directory.sync_all().map_err(unavailable)?;
                } else if std::io::Error::last_os_error().kind()
                    != std::io::ErrorKind::AlreadyExists
                {
                    return Err(CredentialError::Unavailable);
                }
            }
            // SAFETY: openat returns a new owned descriptor; no component follows a symlink.
            let fd = unsafe {
                libc::openat(
                    directory.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                return missing();
            }
            // SAFETY: successful openat transfers this descriptor.
            directory = unsafe { File::from_raw_fd(fd) };
            let metadata = directory.metadata().map_err(unavailable)?;
            // SAFETY: geteuid has no preconditions.
            let owner = unsafe { libc::geteuid() };
            if index + 1 == components.len() {
                private(&directory, true)?;
            } else if (metadata.uid() != owner && metadata.uid() != 0)
                || (metadata.mode() & 0o022 != 0
                    && !(metadata.uid() == 0 && metadata.mode() & 0o1000 != 0))
            {
                return Err(CredentialError::Unavailable);
            }
        }
        Ok(Some(directory))
    }

    fn entry(directory: &File, name: &CStr) -> Result<Option<File>, CredentialError> {
        // SAFETY: the retained directory and terminated leaf name are live. NONBLOCK prevents
        // an unsafe FIFO from blocking before the regular-file check.
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            )
        };
        if fd < 0 {
            return missing();
        }
        // SAFETY: successful openat transfers this descriptor.
        let file = unsafe { File::from_raw_fd(fd) };
        private(&file, false)?;
        Ok(Some(file))
    }
}

impl CredentialStore for PrivateFiles {
    fn kind(&self) -> StoreKind {
        StoreKind::PrivateFile
    }

    fn read(
        &self,
        key: &[u8; 32],
        _: Interaction,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, CredentialError> {
        let Some(directory) = self.directory(false)? else {
            return Ok(None);
        };
        let name = CString::new(account(key)).map_err(unavailable)?;
        let Some(file) = Self::entry(&directory, &name)? else {
            return Ok(None);
        };
        // Bound corrupt input without ever placing secret bytes in a nonzeroizing buffer.
        let mut bytes = Zeroizing::new(Vec::with_capacity(65537));
        file.take(65537)
            .read_to_end(&mut bytes)
            .map_err(unavailable)?;
        if bytes.len() > 65536 {
            return Err(CredentialError::Unavailable);
        }
        Ok(Some(bytes))
    }

    fn insert(&self, key: &[u8; 32], secret: &[u8]) -> Result<(), CredentialError> {
        if secret.len() > 65536 {
            return Err(CredentialError::Unavailable);
        }
        let directory = self.directory(true)?.ok_or(CredentialError::Unavailable)?;
        let name = CString::new(account(key)).map_err(unavailable)?;
        let mut random = [0; 32];
        getrandom::fill(&mut random).map_err(unavailable)?;
        let pending =
            CString::new(format!(".pending-{}", account(&random))).map_err(unavailable)?;
        // SAFETY: exclusive no-follow creation beneath a validated private directory.
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                pending.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return Err(CredentialError::Unavailable);
        }
        // SAFETY: successful openat transfers this descriptor.
        let mut file = unsafe { File::from_raw_fd(fd) };
        let result = (|| {
            private(&file, false)?;
            file.write_all(secret).map_err(unavailable)?;
            file.sync_all().map_err(unavailable)?;
            // SAFETY: both names are leaves of the retained private directory. linkat publishes
            // complete bytes atomically and fails if any destination already exists.
            if unsafe {
                libc::linkat(
                    directory.as_raw_fd(),
                    pending.as_ptr(),
                    directory.as_raw_fd(),
                    name.as_ptr(),
                    0,
                )
            } != 0
            {
                return Err(CredentialError::Unavailable);
            }
            directory.sync_all().map_err(unavailable)
        })();
        let cleanup = unlink(&directory, &pending);
        result.and(cleanup)
    }

    fn remove(&self, key: &[u8; 32], _: Interaction) -> Result<(), CredentialError> {
        let Some(directory) = self.directory(false)? else {
            return Ok(());
        };
        let name = CString::new(account(key)).map_err(unavailable)?;
        // Validate before deleting, including symlink, type, owner and permissions checks.
        if Self::entry(&directory, &name)?.is_some() {
            unlink(&directory, &name)?;
        }
        Ok(())
    }
}

fn private(file: &File, directory: bool) -> Result<(), CredentialError> {
    let metadata = file.metadata().map_err(unavailable)?;
    // SAFETY: geteuid has no preconditions.
    if metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o7777 & !if directory { 0o700 } else { 0o600 } != 0
        || (directory && !metadata.is_dir())
        || (!directory && !metadata.is_file())
    {
        return Err(CredentialError::Unavailable);
    }
    Ok(())
}
fn unlink(directory: &File, name: &CStr) -> Result<(), CredentialError> {
    // SAFETY: a terminated leaf name relative to the retained directory; never follows links.
    if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) } != 0 {
        return Err(CredentialError::Unavailable);
    }
    directory.sync_all().map_err(unavailable)
}
fn missing<T>() -> Result<Option<T>, CredentialError> {
    if std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
        Ok(None)
    } else {
        Err(CredentialError::Unavailable)
    }
}
fn unavailable(_: impl Sized) -> CredentialError {
    CredentialError::Unavailable
}

#[cfg(test)]
mod tests;
