//! Descriptor-relative creation, publication and cleanup beneath the installation lock.
use super::*;
use std::{ffi::CStr, os::unix::fs::PermissionsExt};

pub(super) fn store(path: &Path) -> Result<File, ServerError> {
    if !path.is_absolute() {
        return Err(ServerError::UnsafeStorage);
    }
    let mut directory = File::open("/").map_err(|_| ServerError::StorageUnavailable)?;
    let parts: Vec<_> = path.components().skip(1).collect();
    for (index, part) in parts.iter().enumerate() {
        let std::path::Component::Normal(part) = part else {
            return Err(ServerError::UnsafeStorage);
        };
        let name = CString::new(part.as_bytes()).map_err(|_| ServerError::UnsafeStorage)?;
        // SAFETY: create only a single no-follow component beneath the validated retained parent.
        if unsafe { libc::mkdirat(directory.as_raw_fd(), name.as_ptr(), 0o700) } == 0 {
            directory
                .sync_all()
                .map_err(|_| ServerError::StorageUnavailable)?;
        } else if std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists {
            return Err(ServerError::StorageUnavailable);
        }
        // SAFETY: descriptor and terminated component remain valid; open never follows a symlink.
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(ServerError::UnsafeStorage);
        }
        // SAFETY: openat returned a fresh owned descriptor.
        directory = unsafe { File::from_raw_fd(fd) };
        let metadata = directory
            .metadata()
            .map_err(|_| ServerError::StorageUnavailable)?;
        let last = index + 1 == parts.len();
        // SAFETY: geteuid has no preconditions.
        let account = unsafe { libc::geteuid() };
        let sticky_system_ancestor = !last && metadata.uid() == 0 && metadata.mode() & 0o1000 != 0;
        if (metadata.uid() != account && metadata.uid() != 0)
            || (metadata.mode() & 0o022 != 0 && !sticky_system_ancestor)
            || (last && (metadata.uid() != account || metadata.mode() & 0o7777 != 0o700))
        {
            return Err(ServerError::UnsafeStorage);
        }
    }
    Ok(directory)
}

pub(super) fn create_file(
    directory: &File,
    path: &Path,
    bytes: &[u8],
    mode: u32,
) -> Result<(), ServerError> {
    let parent = create_directories(directory, path.parent().ok_or(ServerError::UnsafeStorage)?)?;
    let name = leaf(path)?;
    // SAFETY: directory and terminated leaf remain live; exclusive creation never replaces an entry.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err(ServerError::StorageUnavailable);
    }
    // SAFETY: openat returned a fresh owned descriptor.
    let mut file = unsafe { File::from_raw_fd(fd) };
    file.write_all(bytes)
        .map_err(|_| ServerError::StorageUnavailable)?;
    file.set_permissions(std::fs::Permissions::from_mode(mode))
        .map_err(|_| ServerError::StorageUnavailable)?;
    file.sync_all()
        .map_err(|_| ServerError::StorageUnavailable)?;
    parent
        .sync_all()
        .map_err(|_| ServerError::StorageUnavailable)
}

pub(super) fn create_directories(directory: &File, path: &Path) -> Result<File, ServerError> {
    let mut current = directory
        .try_clone()
        .map_err(|_| ServerError::StorageUnavailable)?;
    for part in path.components() {
        let std::path::Component::Normal(name) = part else {
            return Err(ServerError::UnsafeStorage);
        };
        let name = CString::new(name.as_bytes()).map_err(|_| ServerError::UnsafeStorage)?;
        // SAFETY: both arguments remain live through descriptor-relative creation.
        if unsafe { libc::mkdirat(current.as_raw_fd(), name.as_ptr(), 0o755) } != 0 {
            if std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists {
                return Err(ServerError::StorageUnavailable);
            }
        } else {
            current
                .sync_all()
                .map_err(|_| ServerError::StorageUnavailable)?;
        }
        current = open_relative(
            &current,
            Path::new(std::ffi::OsStr::from_bytes(name.to_bytes())),
            true,
        )?;
        current
            .set_permissions(std::fs::Permissions::from_mode(0o755))
            .map_err(|_| ServerError::StorageUnavailable)?;
        current
            .sync_all()
            .map_err(|_| ServerError::StorageUnavailable)?;
    }
    Ok(current)
}

pub(super) fn leaf(path: &Path) -> Result<CString, ServerError> {
    let name = path.file_name().ok_or(ServerError::UnsafeStorage)?;
    CString::new(name.as_bytes()).map_err(|_| ServerError::UnsafeStorage)
}

pub(super) fn exists(directory: &File, name: &CStr) -> Result<bool, ServerError> {
    // SAFETY: stat output and descriptor-relative terminated name remain valid.
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

pub(super) struct Preparation {
    pub directory: File,
    parent: File,
    name: CString,
}
impl Preparation {
    pub fn new(parent: &File) -> Result<Self, ServerError> {
        let mut random = [0; 16];
        ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut random)
            .map_err(|_| ServerError::StorageUnavailable)?;
        let name = format!(
            ".prepare-{}",
            random
                .iter()
                .map(|v| format!("{v:02x}"))
                .collect::<String>()
        );
        let name = CString::new(name).map_err(|_| ServerError::UnsafeStorage)?;
        // SAFETY: descriptor and fresh single-component name remain live.
        if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
            return Err(ServerError::StorageUnavailable);
        }
        let directory = open_relative(
            parent,
            Path::new(std::ffi::OsStr::from_bytes(name.to_bytes())),
            true,
        )?;
        Ok(Self {
            directory,
            parent: parent
                .try_clone()
                .map_err(|_| ServerError::StorageUnavailable)?,
            name,
        })
    }
    pub fn path(&self, parent: &Path) -> PathBuf {
        parent.join(std::ffi::OsStr::from_bytes(self.name.to_bytes()))
    }
    pub fn publish(&self, destination: &CStr) -> Result<(), ServerError> {
        revalidate(&self.parent, &self.name, &self.directory)?;
        #[cfg(target_os = "linux")]
        // SAFETY: renameat2 acts on two names beneath the retained directory, refusing replacement.
        let result = unsafe {
            libc::renameat2(
                self.parent.as_raw_fd(),
                self.name.as_ptr(),
                self.parent.as_raw_fd(),
                destination.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        #[cfg(target_os = "macos")]
        // SAFETY: renameatx_np acts on retained directory names, refusing replacement.
        let result = unsafe {
            libc::renameatx_np(
                self.parent.as_raw_fd(),
                self.name.as_ptr(),
                self.parent.as_raw_fd(),
                destination.as_ptr(),
                libc::RENAME_EXCL,
            )
        };
        if result != 0 {
            return Err(ServerError::UnsafeStorage);
        }
        self.parent
            .sync_all()
            .map_err(|_| ServerError::StorageUnavailable)
    }
}
impl Drop for Preparation {
    fn drop(&mut self) {
        // Changed or published names are left alone. Cleanup never follows another directory.
        let _ = remove_tree(&self.parent, &self.name, &self.directory);
    }
}

fn revalidate(parent: &File, name: &CStr, expected: &File) -> Result<(), ServerError> {
    let actual = open_relative(
        parent,
        Path::new(std::ffi::OsStr::from_bytes(name.to_bytes())),
        true,
    )?;
    same(&actual, expected)
}
fn same(actual: &File, expected: &File) -> Result<(), ServerError> {
    let a = actual
        .metadata()
        .map_err(|_| ServerError::StorageUnavailable)?;
    let b = expected
        .metadata()
        .map_err(|_| ServerError::StorageUnavailable)?;
    if a.dev() != b.dev() || a.ino() != b.ino() {
        return Err(ServerError::UnsafeStorage);
    }
    Ok(())
}

pub(super) fn names(directory: &File) -> Result<Vec<OsString>, ServerError> {
    // Open a distinct directory description so each scan starts at the beginning.
    let dot = c".";
    // SAFETY: the retained directory and terminated dot name are valid.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            dot.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(ServerError::StorageUnavailable);
    }
    // SAFETY: fdopendir takes ownership of the fresh descriptor on success.
    let stream = unsafe { libc::fdopendir(fd) };
    if stream.is_null() {
        unsafe {
            libc::close(fd);
        }
        return Err(ServerError::StorageUnavailable);
    }
    struct Stream(*mut libc::DIR);
    impl Drop for Stream {
        fn drop(&mut self) {
            unsafe {
                libc::closedir(self.0);
            }
        }
    }
    let stream = Stream(stream);
    let mut names = Vec::new();
    loop {
        // SAFETY: live directory stream; returned entry is copied before the next call.
        let entry = unsafe { libc::readdir(stream.0) };
        if entry.is_null() {
            break;
        }
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
        if name == c"." || name == c".." {
            continue;
        }
        if names.len() >= 256 {
            return Err(ServerError::UnsafeStorage);
        }
        names.push(std::ffi::OsStr::from_bytes(name.to_bytes()).to_owned());
    }
    Ok(names)
}
fn remove_tree(parent: &File, name: &CStr, directory: &File) -> Result<(), ServerError> {
    revalidate(parent, name, directory)?;
    for entry in names(directory)? {
        let child = open_relative(directory, Path::new(&entry), false)?;
        let metadata = child
            .metadata()
            .map_err(|_| ServerError::StorageUnavailable)?;
        let name = CString::new(entry.as_bytes()).map_err(|_| ServerError::UnsafeStorage)?;
        if metadata.is_dir() {
            remove_tree(directory, &name, &child)?;
        } else if metadata.is_file() && metadata.nlink() == 1 {
            same(&open_relative(directory, Path::new(&entry), false)?, &child)?;
            // SAFETY: single-component name was revalidated immediately before unlinkat.
            if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) } != 0 {
                return Err(ServerError::StorageUnavailable);
            }
        } else {
            return Err(ServerError::UnsafeStorage);
        }
    }
    revalidate(parent, name, directory)?;
    // SAFETY: same retained directory identity was revalidated before removal.
    if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) } != 0 {
        return Err(ServerError::StorageUnavailable);
    }
    parent
        .sync_all()
        .map_err(|_| ServerError::StorageUnavailable)
}
