//! Descriptor-relative private storage. No symlinks, unsafe modes, or foreign owners are repaired.
use crate::ServerError;
#[cfg(any(feature = "client", test))]
use crate::protocol;
use serde::{Deserialize, Serialize};
#[cfg(any(feature = "client", test))]
use std::io::Write;
use std::{
    ffi::CString,
    fs::File,
    io::Read,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Component, Path},
};
use zeroize::Zeroizing;

const MAX_RECORD: usize = 8192;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Record {
    pub seed: [u8; 32],
    pub paired: Vec<[u8; 32]>,
}
impl Drop for Record {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.seed.zeroize();
    }
}

pub(crate) struct Directory(File);
impl Directory {
    pub fn open(path: &Path, create: bool) -> Result<Self, ServerError> {
        if !path.is_absolute() {
            return Err(ServerError::UnsafeStorage);
        }
        let mut dir = File::open("/").map_err(|_| ServerError::StorageUnavailable)?;
        let names: Vec<_> = path
            .components()
            .filter(|v| *v != Component::RootDir)
            .collect();
        for (index, name) in names.iter().enumerate() {
            let Component::Normal(name) = name else {
                return Err(ServerError::UnsafeStorage);
            };
            let name = CString::new(name.as_bytes()).map_err(|_| ServerError::UnsafeStorage)?;
            if create {
                // SAFETY: directory descriptor and NUL-terminated component are live.
                let result = unsafe { libc::mkdirat(dir.as_raw_fd(), name.as_ptr(), 0o700) };
                if result != 0
                    && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists
                {
                    return Err(ServerError::StorageUnavailable);
                }
            }
            // SAFETY: openat receives a live directory and component; successful fd is owned below.
            let fd = unsafe {
                libc::openat(
                    dir.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                return Err(classify_open());
            }
            // SAFETY: successful openat transferred ownership of a fresh descriptor.
            dir = unsafe { File::from_raw_fd(fd) };
            if index + 1 == names.len() {
                private(&dir, true)?;
            }
        }
        Ok(Self(dir))
    }
    fn file(&self, name: &str, create: bool) -> Result<File, ServerError> {
        let name = CString::new(name).map_err(|_| ServerError::UnsafeStorage)?;
        let flags = libc::O_RDWR | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK;
        // Exclusive creation makes concurrent setup deterministic on macOS, where competing
        // non-exclusive O_CREAT opens can return ENOENT even while the directory is retained.
        // SAFETY: directory descriptor and leaf name remain live; returned fd is owned below.
        let mut fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                name.as_ptr(),
                flags
                    | if create {
                        libc::O_CREAT | libc::O_EXCL
                    } else {
                        0
                    },
                0o600,
            )
        };
        if fd < 0
            && create
            && std::io::Error::last_os_error().kind() == std::io::ErrorKind::AlreadyExists
        {
            // SAFETY: this only opens an existing no-follow leaf beneath the retained directory.
            fd = unsafe { libc::openat(self.0.as_raw_fd(), name.as_ptr(), flags) };
        }
        if fd < 0 {
            return Err(classify_open());
        }
        // SAFETY: successful openat transferred a fresh descriptor.
        let file = unsafe { File::from_raw_fd(fd) };
        private(&file, false)?;
        Ok(file)
    }
    pub fn lock(&self, name: &str, nonblocking: bool) -> Result<File, ServerError> {
        let file = self.file(name, true)?;
        // SAFETY: flock acts only on the supplied live descriptor.
        if unsafe {
            libc::flock(
                file.as_raw_fd(),
                libc::LOCK_EX | if nonblocking { libc::LOCK_NB } else { 0 },
            )
        } != 0
        {
            return Err(ServerError::StorageUnavailable);
        }
        Ok(file)
    }
    pub fn read(&self) -> Result<Record, ServerError> {
        let file = self.file("identity.json", false)?;
        let mut bytes = Zeroizing::new(Vec::new());
        file.take((MAX_RECORD + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| ServerError::StorageUnavailable)?;
        if bytes.len() > MAX_RECORD {
            return Err(ServerError::UnsafeStorage);
        }
        let record: Record =
            serde_json::from_slice(&bytes).map_err(|_| ServerError::UnsafeStorage)?;
        if record.paired.len() > 64 {
            return Err(ServerError::UnsafeStorage);
        }
        Ok(record)
    }
    #[cfg(any(feature = "client", test))]
    pub fn read_or_create(&self) -> Result<Record, ServerError> {
        match self.read() {
            Ok(record) => Ok(record),
            Err(ServerError::NotPaired) => {
                let record = Record {
                    seed: protocol::random()?,
                    paired: Vec::new(),
                };
                self.write(&record)?;
                Ok(record)
            }
            Err(error) => Err(error),
        }
    }
    /// Caller holds setup.lock. Publish once after fsync so crashes never truncate the old identity.
    #[cfg(any(feature = "client", test))]
    pub fn write(&self, record: &Record) -> Result<(), ServerError> {
        let bytes = Zeroizing::new(
            serde_json::to_vec(record).map_err(|_| ServerError::StorageUnavailable)?,
        );
        if bytes.len() > MAX_RECORD {
            return Err(ServerError::StorageUnavailable);
        }
        let name = format!(
            "pending-{}",
            u128::from_be_bytes(
                protocol::random()?[..16]
                    .try_into()
                    .map_err(|_| ServerError::StorageUnavailable)?
            )
        );
        let c_name = CString::new(name.as_str()).map_err(|_| ServerError::StorageUnavailable)?;
        // SAFETY: openat creates a unique private regular file beneath the retained directory.
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                c_name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return Err(ServerError::StorageUnavailable);
        }
        // SAFETY: successful openat transferred ownership of this descriptor.
        let mut file = unsafe { File::from_raw_fd(fd) };
        let result = (|| {
            file.write_all(&bytes)
                .and_then(|()| file.sync_all())
                .map_err(|_| ServerError::StorageUnavailable)?;
            // SAFETY: both names are single components relative to the same live directory.
            if unsafe {
                libc::renameat(
                    self.0.as_raw_fd(),
                    c_name.as_ptr(),
                    self.0.as_raw_fd(),
                    c"identity.json".as_ptr(),
                )
            } != 0
            {
                return Err(ServerError::StorageUnavailable);
            }
            self.0
                .sync_all()
                .map_err(|_| ServerError::StorageUnavailable)
        })();
        // SAFETY: cleanup removes only the unique temporary name; rename may already have removed it.
        unsafe {
            libc::unlinkat(self.0.as_raw_fd(), c_name.as_ptr(), 0);
        }
        result
    }
}
fn private(file: &File, directory: bool) -> Result<(), ServerError> {
    let meta = file
        .metadata()
        .map_err(|_| ServerError::StorageUnavailable)?;
    // SAFETY: geteuid has no preconditions.
    if meta.uid() != unsafe { libc::geteuid() }
        || meta.mode() & 0o7777 != if directory { 0o700 } else { 0o600 }
        || if directory {
            !meta.is_dir()
        } else {
            !meta.is_file() || meta.nlink() != 1
        }
    {
        return Err(ServerError::UnsafeStorage);
    }
    Ok(())
}
fn classify_open() -> ServerError {
    match std::io::Error::last_os_error().raw_os_error() {
        Some(libc::ENOENT) => ServerError::NotPaired,
        Some(libc::ELOOP | libc::ENOTDIR) => ServerError::UnsafeStorage,
        _ => ServerError::StorageUnavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::{PermissionsExt, symlink},
        path::PathBuf,
    };
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
                "spaceterm-storage-{}",
                u128::from_be_bytes(protocol::random().unwrap()[..16].try_into().unwrap())
            ));
            Self(path)
        }
        fn open(&self) -> Directory {
            Directory::open(&self.0, true).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn repeated_setup_retains_identity_and_paired_devices_with_private_modes() {
        let fixture = Fixture::new();
        let data = fixture.open();
        let _lock = data.lock("setup.lock", false).unwrap();
        let mut record = data.read_or_create().unwrap();
        let original = record.seed;
        record.paired.push([42; 32]);
        data.write(&record).unwrap();
        let restored = data.read_or_create().unwrap();
        assert_eq!(
            (restored.seed, restored.paired.as_slice()),
            (original, [[42; 32]].as_slice())
        );
        assert_eq!(fs::metadata(&fixture.0).unwrap().mode() & 0o7777, 0o700);
        assert_eq!(
            fs::metadata(fixture.0.join("identity.json"))
                .unwrap()
                .mode()
                & 0o7777,
            0o600
        );
    }
    #[test]
    fn corrupt_identity_is_preserved_instead_of_replaced() {
        let fixture = Fixture::new();
        let data = fixture.open();
        let _lock = data.lock("setup.lock", false).unwrap();
        let mut file = data.file("identity.json", true).unwrap();
        file.write_all(b"interrupted or unsupported identity")
            .unwrap();
        assert!(matches!(
            data.read_or_create(),
            Err(ServerError::UnsafeStorage)
        ));
        assert_eq!(
            fs::read(fixture.0.join("identity.json")).unwrap(),
            b"interrupted or unsupported identity"
        );
    }
    #[test]
    fn refuses_symlinked_or_world_readable_identity() {
        let fixture = Fixture::new();
        let data = fixture.open();
        let _lock = data.lock("setup.lock", false).unwrap();
        data.read_or_create().unwrap();
        let path = fixture.0.join("identity.json");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(data.read(), Err(ServerError::UnsafeStorage)));
        fs::rename(&path, fixture.0.join("other")).unwrap();
        symlink("other", &path).unwrap();
        assert!(matches!(
            data.read_or_create(),
            Err(ServerError::UnsafeStorage)
        ));
    }
    #[test]
    fn concurrent_setup_serializes_identity_creation() {
        let fixture = Fixture::new();
        fixture.open();
        let mut threads = Vec::new();
        for _ in 0..4 {
            let path = fixture.0.clone();
            threads.push(std::thread::spawn(move || {
                let directory = Directory::open(&path, true).unwrap();
                let _lock = directory.lock("setup.lock", false).unwrap();
                directory.read_or_create().unwrap().seed
            }));
        }
        let keys: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        assert!(keys.iter().all(|key| *key == keys[0]));
    }
}
