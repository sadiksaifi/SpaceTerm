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
    pub fn install(&self, expected: &[u8]) -> Result<(), ServerError> {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.directory)
            .map_err(|_| ServerError::StorageUnavailable)?;
        let directory = owned_directory(&self.directory)?;
        if exists(&directory, &self.name)? {
            return self.validate(&directory, expected).map(|_| ());
        }
        let temporary = format!(
            ".prepare-{:x}",
            u128::from_be_bytes(
                crate::protocol::random()?[..16]
                    .try_into()
                    .map_err(|_| ServerError::StorageUnavailable)?
            )
        );
        let name = CString::new(self.name.as_bytes()).map_err(|_| ServerError::UnsafeStorage)?;
        let pending = CString::new(temporary.as_bytes()).map_err(|_| ServerError::UnsafeStorage)?;
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
            return Err(ServerError::StorageUnavailable);
        }
        // SAFETY: openat transferred a fresh descriptor.
        let mut file = unsafe { File::from_raw_fd(fd) };
        let result = (|| {
            file.write_all(expected)
                .and_then(|()| file.sync_all())
                .map_err(|_| ServerError::StorageUnavailable)?;
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
                return Err(ServerError::CleanupUnresolved);
            }
            directory
                .sync_all()
                .map_err(|_| ServerError::StorageUnavailable)
        })();
        // SAFETY: this operation removes only the unpredictable temporary leaf it created.
        unsafe { libc::unlinkat(directory.as_raw_fd(), pending.as_ptr(), 0) };
        directory
            .sync_all()
            .map_err(|_| ServerError::StorageUnavailable)?;
        result
    }
    fn validate(&self, directory: &File, expected: &[u8]) -> Result<File, ServerError> {
        let file = open_relative(directory, Path::new(&self.name), false)?;
        let metadata = file
            .metadata()
            .map_err(|_| ServerError::StorageUnavailable)?;
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err(ServerError::UnsafeStorage);
        }
        let mut bytes = Vec::new();
        (&file)
            .take(65537)
            .read_to_end(&mut bytes)
            .map_err(|_| ServerError::StorageUnavailable)?;
        if bytes != expected {
            return Err(ServerError::CleanupUnresolved);
        }
        Ok(file)
    }
    pub fn check(&self, expected: &[u8], missing_allowed: bool) -> Result<(), ServerError> {
        let directory = owned_directory(&self.directory)?;
        if missing_allowed && !exists(&directory, &self.name)? {
            return Ok(());
        }
        self.validate(&directory, expected).map(|_| ())
    }
    pub fn remove(&self, expected: &[u8]) -> Result<(), ServerError> {
        let directory = owned_directory(&self.directory)?;
        // Missing is idempotent only after the caller durably recorded removal intent.
        if !exists(&directory, &self.name)? {
            return Ok(());
        }
        let file = self.validate(&directory, expected)?;
        let before = file
            .metadata()
            .map_err(|_| ServerError::StorageUnavailable)?;
        verify_directory_path(&directory, &self.directory)?;
        let current = open_relative(&directory, Path::new(&self.name), false)?
            .metadata()
            .map_err(|_| ServerError::StorageUnavailable)?;
        if (before.dev(), before.ino()) != (current.dev(), current.ino()) {
            return Err(ServerError::CleanupUnresolved);
        }
        let name = CString::new(self.name.as_bytes()).map_err(|_| ServerError::UnsafeStorage)?;
        // SAFETY: delete only the just-revalidated entry beneath the retained directory.
        if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) } != 0 {
            return Err(ServerError::StorageUnavailable);
        }
        directory
            .sync_all()
            .map_err(|_| ServerError::StorageUnavailable)
    }
}
fn exists(directory: &File, name: &str) -> Result<bool, ServerError> {
    let name = CString::new(name.as_bytes()).map_err(|_| ServerError::UnsafeStorage)?;
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
        Err(ServerError::StorageUnavailable)
    }
}
