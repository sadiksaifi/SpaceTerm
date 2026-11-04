//! Publish a service definition without replacing foreign or changed entries.
use super::*;
use std::os::unix::fs::DirBuilderExt;

pub(super) struct Registration {
    pub directory: PathBuf,
    pub name: String,
}
impl Registration {
    pub fn path(&self) -> PathBuf {
        self.directory.join(&self.name)
    }
    pub fn install(&self, expected: &[u8]) -> Result<(), CoreError> {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.directory)
            .map_err(|_| CoreError::StorageUnavailable)?;
        let directory = owned_directory(&self.directory)?;
        if exists(&directory, &self.name)? {
            return self.validate(&directory, expected).map(|_| ());
        }
        let temporary = format!(
            ".prepare-{:x}",
            u128::from_be_bytes(
                crate::protocol::random()?[..16]
                    .try_into()
                    .map_err(|_| CoreError::StorageUnavailable)?
            )
        );
        let name = CString::new(self.name.as_bytes()).map_err(|_| CoreError::UnsafeStorage)?;
        let pending = CString::new(temporary.as_bytes()).map_err(|_| CoreError::UnsafeStorage)?;
        // SAFETY: exclusive creation beneath a retained account directory cannot replace a leaf.
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                pending.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return Err(CoreError::StorageUnavailable);
        }
        // SAFETY: openat transferred a fresh descriptor.
        let mut file = unsafe { File::from_raw_fd(fd) };
        let result = (|| {
            file.write_all(expected)
                .and_then(|()| file.sync_all())
                .map_err(|_| CoreError::StorageUnavailable)?;
            verify_directory_path(&directory, &self.directory)?;
            // SAFETY: linkat publishes complete bytes exclusively, without overwriting a target.
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
                return Err(CoreError::CleanupUnresolved);
            }
            directory
                .sync_all()
                .map_err(|_| CoreError::StorageUnavailable)
        })();
        // SAFETY: this operation removes only the unpredictable temporary leaf it created.
        unsafe { libc::unlinkat(directory.as_raw_fd(), pending.as_ptr(), 0) };
        directory
            .sync_all()
            .map_err(|_| CoreError::StorageUnavailable)?;
        result
    }
    fn validate(&self, directory: &File, expected: &[u8]) -> Result<File, CoreError> {
        let file = open_relative(directory, Path::new(&self.name), false)?;
        let metadata = file.metadata().map_err(|_| CoreError::StorageUnavailable)?;
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err(CoreError::UnsafeStorage);
        }
        let mut bytes = Vec::new();
        (&file)
            .take(65537)
            .read_to_end(&mut bytes)
            .map_err(|_| CoreError::StorageUnavailable)?;
        if bytes != expected {
            return Err(CoreError::CleanupUnresolved);
        }
        Ok(file)
    }
    pub fn check(&self, expected: &[u8], missing_allowed: bool) -> Result<(), CoreError> {
        let directory = owned_directory(&self.directory)?;
        if missing_allowed && !exists(&directory, &self.name)? {
            return Ok(());
        }
        self.validate(&directory, expected).map(|_| ())
    }
    /// Replace only the original admitted bytes, or synchronize that exact replacement again.
    pub fn replace(&self, previous: &[u8], candidate: &[u8]) -> Result<(), CoreError> {
        let directory = owned_directory(&self.directory)?;
        if self.validate(&directory, candidate).is_ok() {
            return directory
                .sync_all()
                .map_err(|_| CoreError::StorageUnavailable);
        }
        let original = self.validate(&directory, previous)?;
        let before = original
            .metadata()
            .map_err(|_| CoreError::StorageUnavailable)?;
        let temporary = format!(
            ".replace-{:x}",
            u128::from_be_bytes(
                crate::protocol::random()?[..16]
                    .try_into()
                    .map_err(|_| CoreError::StorageUnavailable)?
            )
        );
        let pending = CString::new(temporary.as_bytes()).map_err(|_| CoreError::UnsafeStorage)?;
        let name = CString::new(self.name.as_bytes()).map_err(|_| CoreError::UnsafeStorage)?;
        // SAFETY: exclusively create one unpredictable leaf under the retained owned directory.
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                pending.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return Err(CoreError::StorageUnavailable);
        }
        // SAFETY: openat transferred a new owned descriptor.
        let mut file = unsafe { File::from_raw_fd(fd) };
        let result = (|| {
            file.write_all(candidate)
                .and_then(|()| file.sync_all())
                .map_err(|_| CoreError::StorageUnavailable)?;
            verify_directory_path(&directory, &self.directory)?;
            let current = self
                .validate(&directory, previous)?
                .metadata()
                .map_err(|_| CoreError::StorageUnavailable)?;
            if (before.dev(), before.ino()) != (current.dev(), current.ino()) {
                return Err(CoreError::CleanupUnresolved);
            }
            // SAFETY: replace the just-revalidated owned leaf with the complete synced file.
            if unsafe {
                libc::renameat(
                    directory.as_raw_fd(),
                    pending.as_ptr(),
                    directory.as_raw_fd(),
                    name.as_ptr(),
                )
            } != 0
            {
                return Err(CoreError::StorageUnavailable);
            }
            directory
                .sync_all()
                .map_err(|_| CoreError::StorageUnavailable)
        })();
        // SAFETY: cleanup only the temporary name created by this call.
        unsafe { libc::unlinkat(directory.as_raw_fd(), pending.as_ptr(), 0) };
        result
    }
    pub fn remove(&self, expected: &[u8]) -> Result<(), CoreError> {
        let directory = owned_directory(&self.directory)?;
        // Missing is idempotent only after the caller durably recorded removal intent.
        if !exists(&directory, &self.name)? {
            return Ok(());
        }
        let file = self.validate(&directory, expected)?;
        let before = file.metadata().map_err(|_| CoreError::StorageUnavailable)?;
        verify_directory_path(&directory, &self.directory)?;
        let current = open_relative(&directory, Path::new(&self.name), false)?
            .metadata()
            .map_err(|_| CoreError::StorageUnavailable)?;
        if (before.dev(), before.ino()) != (current.dev(), current.ino()) {
            return Err(CoreError::CleanupUnresolved);
        }
        let name = CString::new(self.name.as_bytes()).map_err(|_| CoreError::UnsafeStorage)?;
        // SAFETY: delete only the just-revalidated entry beneath the retained directory.
        if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) } != 0 {
            return Err(CoreError::StorageUnavailable);
        }
        directory
            .sync_all()
            .map_err(|_| CoreError::StorageUnavailable)
    }
}
fn exists(directory: &File, name: &str) -> Result<bool, CoreError> {
    let name = CString::new(name.as_bytes()).map_err(|_| CoreError::UnsafeStorage)?;
    // SAFETY: zeroed stat is writable output for the retained descriptor and leaf name.
    let mut metadata: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe {
        libc::fstatat(
            directory.as_raw_fd(),
            name.as_ptr(),
            &mut metadata,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } == 0
    {
        return Ok(true);
    }
    if std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
        Ok(false)
    } else {
        Err(CoreError::StorageUnavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publication_and_replacement_reuse_exact_bytes_and_preserve_foreign_entries() {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "sts-registration-{:x}",
            u128::from_be_bytes(crate::protocol::random().unwrap()[..16].try_into().unwrap())
        ));
        Directory::open(&root, true).unwrap();
        let registration = Registration {
            directory: root.clone(),
            name: "service".into(),
        };
        registration.install(b"original").unwrap();
        registration.install(b"original").unwrap();
        registration.replace(b"original", b"candidate").unwrap();
        registration.replace(b"original", b"candidate").unwrap();
        assert_eq!(std::fs::read(registration.path()).unwrap(), b"candidate");
        std::fs::write(registration.path(), b"foreign").unwrap();
        assert_eq!(
            registration.remove(b"original"),
            Err(CoreError::CleanupUnresolved)
        );
        assert_eq!(
            registration.replace(b"original", b"candidate"),
            Err(CoreError::CleanupUnresolved)
        );
        assert_eq!(std::fs::read(registration.path()).unwrap(), b"foreign");
        std::fs::remove_file(registration.path()).unwrap();
        std::os::unix::fs::symlink("unchanged-foreign-target", registration.path()).unwrap();
        assert!(registration.replace(b"original", b"candidate").is_err());
        assert!(registration.install(b"original").is_err());
        assert!(
            std::fs::symlink_metadata(registration.path())
                .unwrap()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read_link(registration.path()).unwrap(),
            Path::new("unchanged-foreign-target")
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
