use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::fs::File;
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};

use super::StorageFailure;
use super::locking::Locks;

pub(super) fn mutex<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(super) fn io_failure() -> StorageFailure {
    match std::io::Error::last_os_error().raw_os_error() {
        Some(libc::ELOOP | libc::ENOTDIR) => StorageFailure::UnsafeStorage,
        Some(
            libc::EACCES
            | libc::EPERM
            | libc::ENOENT
            | libc::ENODEV
            | libc::ENXIO
            | libc::EBUSY
            | libc::EAGAIN
            | libc::ENOLCK
            | libc::ENOSPC
            | libc::EDQUOT
            | libc::ETIMEDOUT
            | libc::EROFS,
        ) => StorageFailure::Unavailable,
        _ => StorageFailure::Io,
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) struct Identity {
    device: u64,
    inode: u64,
}

impl Identity {
    #[expect(
        clippy::unnecessary_cast,
        reason = "native stat field widths differ across hosts"
    )]
    fn of(stat: &libc::stat) -> Self {
        Self {
            device: stat.st_dev as u64,
            inode: stat.st_ino as u64,
        }
    }
}

fn stat_fd(fd: RawFd) -> Result<libc::stat, StorageFailure> {
    // SAFETY: stat is initialized by fstat before it is inspected.
    let mut stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut stat) } != 0 {
        return Err(io_failure());
    }
    Ok(stat)
}

fn stat_at(fd: RawFd, name: &CStr) -> Result<Option<libc::stat>, StorageFailure> {
    // SAFETY: name is terminated, and stat has space for a native stat structure.
    let mut stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstatat(fd, name.as_ptr(), &mut stat, libc::AT_SYMLINK_NOFOLLOW) } == 0 {
        return Ok(Some(stat));
    }
    if std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT) {
        Ok(None)
    } else {
        Err(io_failure())
    }
}

fn private_directory(stat: &libc::stat) -> Result<(), StorageFailure> {
    // SAFETY: geteuid has no pointer arguments.
    if stat.st_mode & libc::S_IFMT != libc::S_IFDIR
        || stat.st_uid != unsafe { libc::geteuid() }
        || stat.st_mode & 0o7777 != 0o700
    {
        return Err(StorageFailure::UnsafeStorage);
    }
    Ok(())
}

fn storage_filesystem(fd: RawFd) -> Result<(), StorageFailure> {
    // SAFETY: fstatfs initializes the native structure for this retained descriptor.
    let mut filesystem: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstatfs(fd, &mut filesystem) } != 0 {
        return Err(io_failure());
    }
    #[cfg(target_os = "linux")]
    {
        // Local ext-family, XFS, and Btrfs implement the required file/directory
        // sync and POSIX locking semantics. Unknown, network, FUSE, overlay, and
        // volatile filesystems are not admitted merely because fsync returns OK.
        if !matches!(filesystem.f_type, 0xef53 | 0x5846_5342 | 0x9123_683e) {
            return Err(StorageFailure::UnsafeStorage);
        }
    }
    #[cfg(target_os = "macos")]
    {
        let name: Vec<_> = filesystem
            .f_fstypename
            .iter()
            .take_while(|byte| **byte != 0)
            .map(|byte| *byte as u8)
            .collect();
        if !matches!(name.as_slice(), b"apfs" | b"hfs")
            || filesystem.f_flags & libc::MNT_LOCAL as u32 == 0
            || filesystem.f_flags & libc::MNT_IGNORE_OWNERSHIP as u32 != 0
        {
            return Err(StorageFailure::UnsafeStorage);
        }
    }
    Ok(())
}

fn private_file(stat: &libc::stat) -> Result<(), StorageFailure> {
    // SAFETY: geteuid has no pointer arguments.
    if stat.st_mode & libc::S_IFMT != libc::S_IFREG
        || stat.st_uid != unsafe { libc::geteuid() }
        || stat.st_mode & 0o7777 != 0o600
        || stat.st_nlink != 1
    {
        return Err(StorageFailure::UnsafeStorage);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn private_acl(_fd: RawFd) -> Result<(), StorageFailure> {
    // POSIX ACL permissions are restricted by the mode's group mask. Modes
    // 0700/0600 exclude effective named-user/group grants on admitted Linux filesystems.
    Ok(())
}

#[cfg(target_os = "macos")]
fn private_acl(fd: RawFd) -> Result<(), StorageFailure> {
    use std::ffi::{c_int, c_void};
    unsafe extern "C" {
        fn acl_get_fd_np(fd: c_int, kind: c_int) -> *mut c_void;
        fn acl_valid(acl: *mut c_void) -> c_int;
        fn acl_get_entry(acl: *mut c_void, entry_id: c_int, entry: *mut *mut c_void) -> c_int;
        fn acl_free(acl: *mut c_void) -> c_int;
    }
    // macOS extended ACLs grant access independently of mode bits. Reject any
    // entry rather than attempting to infer principal or inheritance policy.
    // These APIs inspect the existing descriptor; they never reopen a path.
    // SAFETY: ACL_TYPE_EXTENDED is the native type, and fd is retained.
    let acl = unsafe { acl_get_fd_np(fd, 0x100) };
    if acl.is_null() {
        // acl_get_fd_np uses fstatx_np on this descriptor. Its FILESEC_ACL
        // property reports ENOENT for confirmed absence, not ENOTSUP/denial.
        if std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT) {
            return Ok(());
        }
        return Err(io_failure());
    }
    // SAFETY: the returned allocation is valid until the one acl_free below.
    let valid = unsafe { acl_valid(acl) };
    let mut entry = std::ptr::null_mut();
    let result = unsafe { acl_get_entry(acl, 0, &mut entry) };
    let errno = std::io::Error::last_os_error().raw_os_error();
    let freed = unsafe { acl_free(acl) };
    // Apple's acl_get_entry returns 0 for an entry, and -1/EINVAL for the
    // end of a VALID ACL. Validate first so malformed input cannot look empty.
    if valid != 0 || freed != 0 || result != -1 || errno != Some(libc::EINVAL) {
        return Err(StorageFailure::UnsafeStorage);
    }
    Ok(())
}

struct Ancestor {
    file: File,
    identity: Identity,
    name: Option<CString>,
}

pub(super) struct Directory {
    ancestors: Vec<Ancestor>,
    // Serialize namespace observations/cache updates with create/delete. Acquire
    // this mutex before the inode registry, never in the opposite order.
    expected: Mutex<[Option<Identity>; 4]>,
    #[cfg(test)]
    pub(super) journal_observation: Mutex<Option<JournalObservation>>,
}

#[cfg(test)]
pub(super) struct JournalObservation {
    pub(super) sampled: std::sync::mpsc::SyncSender<bool>,
    pub(super) resume: std::sync::mpsc::Receiver<()>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum Kind {
    Database = 0,
    Journal = 1,
    Wal = 2,
    Shm = 3,
}

impl Kind {
    pub(super) fn name(self) -> &'static CStr {
        match self {
            Self::Database => c"organization.sqlite3",
            Self::Journal => c"organization.sqlite3-journal",
            Self::Wal => c"organization.sqlite3-wal",
            Self::Shm => c"organization.sqlite3-shm",
        }
    }
}

static DIRECTORIES: OnceLock<Mutex<HashMap<Identity, Weak<Directory>>>> = OnceLock::new();
static FILES: OnceLock<Mutex<HashMap<Identity, Weak<Entry>>>> = OnceLock::new();

fn files() -> &'static Mutex<HashMap<Identity, Weak<Entry>>> {
    FILES.get_or_init(Mutex::default)
}

impl Directory {
    pub(super) fn open(path: &Path) -> Result<Arc<Self>, StorageFailure> {
        if !path.is_absolute()
            || path
                .as_os_str()
                .as_bytes()
                .split(|byte| *byte == b'/')
                .any(|part| part == b"." || part == b"..")
        {
            return Err(StorageFailure::UnsafeStorage);
        }
        // SAFETY: the literal is terminated, and open returns an owned descriptor.
        let fd = unsafe {
            libc::open(
                c"/".as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io_failure());
        }
        // SAFETY: fd is newly opened and has exactly one File owner.
        let root = unsafe { File::from_raw_fd(fd) };
        let root_identity = Identity::of(&stat_fd(fd)?);
        let mut ancestors = vec![Ancestor {
            file: root,
            identity: root_identity,
            name: None,
        }];
        for component in path.components() {
            let Component::Normal(name) = component else {
                if component == Component::RootDir {
                    continue;
                }
                return Err(StorageFailure::UnsafeStorage);
            };
            if ancestors.len() >= 256 {
                return Err(StorageFailure::UnsafeStorage);
            }
            let name = CString::new(name.as_bytes()).map_err(|_| StorageFailure::UnsafeStorage)?;
            let parent = ancestors
                .last()
                .ok_or(StorageFailure::UnsafeStorage)?
                .file
                .as_raw_fd();
            // SAFETY: parent is retained and name is a single terminated component.
            let fd = unsafe {
                libc::openat(
                    parent,
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                )
            };
            if fd < 0 {
                // A no-follow directory open may report ENOTDIR for a symlink.
                if stat_at(parent, &name)?
                    .is_some_and(|stat| stat.st_mode & libc::S_IFMT == libc::S_IFLNK)
                {
                    return Err(StorageFailure::UnsafeStorage);
                }
                return Err(io_failure());
            }
            // SAFETY: fd is newly opened and uniquely owned.
            let file = unsafe { File::from_raw_fd(fd) };
            let identity = Identity::of(&stat_fd(fd)?);
            ancestors.push(Ancestor {
                file,
                identity,
                name: Some(name),
            });
        }
        let directory = Arc::new(Self {
            ancestors,
            expected: Mutex::new([None; 4]),
            #[cfg(test)]
            journal_observation: Mutex::new(None),
        });
        directory.verify()?;
        let mut directories = mutex(DIRECTORIES.get_or_init(Mutex::default));
        directories.retain(|_, value| value.strong_count() != 0);
        let identity = directory
            .ancestors
            .last()
            .ok_or(StorageFailure::UnsafeStorage)?
            .identity;
        if let Some(existing) = directories.get(&identity).and_then(Weak::upgrade) {
            existing.verify()?;
            return Ok(existing);
        }
        directories.insert(identity, Arc::downgrade(&directory));
        Ok(directory)
    }

    fn fd(&self) -> RawFd {
        self.ancestors
            .last()
            .map_or(-1, |ancestor| ancestor.file.as_raw_fd())
    }

    pub(super) fn identity(&self) -> Result<Identity, StorageFailure> {
        self.verify()?;
        self.ancestors
            .last()
            .map(|ancestor| ancestor.identity)
            .ok_or(StorageFailure::UnsafeStorage)
    }

    pub(super) fn verify(&self) -> Result<(), StorageFailure> {
        for (index, ancestor) in self.ancestors.iter().enumerate() {
            let stat = stat_fd(ancestor.file.as_raw_fd())?;
            if Identity::of(&stat) != ancestor.identity
                || stat.st_mode & libc::S_IFMT != libc::S_IFDIR
            {
                return Err(StorageFailure::UnsafeStorage);
            }
            if let Some(name) = &ancestor.name {
                let linked = stat_at(self.ancestors[index - 1].file.as_raw_fd(), name)?;
                if linked.is_none_or(|stat| {
                    Identity::of(&stat) != ancestor.identity
                        || stat.st_mode & libc::S_IFMT != libc::S_IFDIR
                }) {
                    return Err(StorageFailure::UnsafeStorage);
                }
            }
        }
        private_directory(&stat_fd(self.fd())?)?;
        storage_filesystem(self.fd())?;
        private_acl(self.fd())
    }

    pub(super) fn check(&self, kind: Kind) -> Result<Option<Identity>, StorageFailure> {
        self.verify()?;
        self.check_locked(kind, &mut mutex(&self.expected))
    }

    fn check_locked(
        &self,
        kind: Kind,
        expected: &mut [Option<Identity>; 4],
    ) -> Result<Option<Identity>, StorageFailure> {
        let observed = stat_at(self.fd(), kind.name())?;
        #[cfg(test)]
        if kind == Kind::Journal && observed.is_some() {
            // Pause at the native observation boundary. The test selects a
            // schedule without timeouts: let an unguarded deletion finish first,
            // or release a guarded observation before waiting for deletion.
            let pause = mutex(&self.journal_observation).take();
            if let Some(pause) = pause {
                let guarded = self.expected.try_lock().is_err();
                if pause.sampled.send(guarded).is_ok() {
                    let _ = pause.resume.recv();
                }
            }
        }
        let identity = match observed {
            Some(stat) => {
                private_file(&stat)?;
                let identity = Identity::of(&stat);
                if expected[kind as usize].is_some_and(|value| value != identity) {
                    return Err(StorageFailure::UnsafeStorage);
                }
                expected[kind as usize] = Some(identity);
                Some(identity)
            }
            None if expected[kind as usize].is_some() => return Err(StorageFailure::UnsafeStorage),
            None => None,
        };
        #[cfg(target_os = "macos")]
        if let Some(identity) = identity {
            // Inspect a live inode ONLY through its already-owned VFS descriptor.
            // The namespace mutex precedes the registry in the lock order.
            let entry = mutex(files()).get(&identity).and_then(Weak::upgrade);
            if let Some(entry) = entry {
                private_acl(entry.fd())?;
            }
        }
        Ok(identity)
    }

    pub(super) fn check_all(&self) -> Result<(), StorageFailure> {
        self.check(Kind::Database)?;
        self.check(Kind::Journal)?;
        for kind in [Kind::Wal, Kind::Shm] {
            if self.check(kind)?.is_some() {
                // WAL is deliberately unsupported. Never convert or discard it.
                return Err(StorageFailure::InvalidState);
            }
        }
        Ok(())
    }

    pub(super) fn sync(&self) -> Result<(), StorageFailure> {
        self.verify()?;
        // SAFETY: fd remains a retained directory descriptor.
        if unsafe { libc::fsync(self.fd()) } != 0 {
            return Err(io_failure());
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn open_entry(
        self: &Arc<Self>,
        kind: Kind,
        create: bool,
    ) -> Result<Arc<Entry>, StorageFailure> {
        self.open_entry_with_evidence(kind, create)
            .map(|(entry, _)| entry)
    }

    pub(super) fn open_entry_with_evidence(
        self: &Arc<Self>,
        kind: Kind,
        create: bool,
    ) -> Result<(Arc<Entry>, bool), StorageFailure> {
        if !matches!(kind, Kind::Database | Kind::Journal) {
            return Err(StorageFailure::InvalidState);
        }
        self.verify()?;
        let mut expected = mutex(&self.expected);
        let before = self.check_locked(kind, &mut expected)?;
        let mut registry = mutex(files());
        if let Some(weak) = before.and_then(|identity| registry.get(&identity)) {
            // A final close may be waiting for this mutex. Do not reopen until
            // that close has removed the inode from the registry.
            let entry = weak.upgrade().ok_or(StorageFailure::Unavailable)?;
            drop(registry);
            drop(expected);
            if entry.kind != kind {
                return Err(StorageFailure::UnsafeStorage);
            }
            entry.verify(self)?;
            return Ok((entry, false));
        }
        let flags = libc::O_RDWR | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK;
        let flags = if before.is_none() && create {
            flags | libc::O_CREAT | libc::O_EXCL
        } else {
            flags
        };
        // SAFETY: all access is relative to the retained directory, with no-follow flags.
        let fd = unsafe { libc::openat(self.fd(), kind.name().as_ptr(), flags, 0o600) };
        if fd < 0 {
            return Err(io_failure());
        }
        // SAFETY: the returned descriptor has one owner until enrolled below.
        let file = unsafe { File::from_raw_fd(fd) };
        let stat = stat_fd(fd)?;
        let identity = Identity::of(&stat);
        // A raced alias must not be closed while that inode carries POSIX locks.
        // Retain even a rejected duplicate until its existing SQLite handles close.
        if let Some(weak) = registry.get(&identity) {
            if let Some(entry) = weak.upgrade() {
                mutex(&entry.duplicates).push(file);
                drop(registry);
                drop(expected);
                drop(entry);
            } else {
                // No locks remain, but serialize this close with the final close.
                drop(file);
            }
            return Err(StorageFailure::UnsafeStorage);
        }
        private_file(&stat)?;
        private_acl(fd)?;
        if before.is_some_and(|value| value != identity) {
            return Err(StorageFailure::UnsafeStorage);
        }
        let linked = stat_at(self.fd(), kind.name())?.ok_or(StorageFailure::UnsafeStorage)?;
        private_file(&linked)?;
        if Identity::of(&linked) != identity {
            return Err(StorageFailure::UnsafeStorage);
        }
        expected[kind as usize] = Some(identity);
        let entry = Arc::new(Entry {
            file: Some(file),
            identity,
            kind,
            locks: Mutex::default(),
            duplicates: Mutex::default(),
        });
        registry.insert(identity, Arc::downgrade(&entry));
        drop(registry);
        drop(expected);
        if before.is_none() {
            self.sync()?;
        }
        Ok((entry, before.is_none() && create))
    }

    pub(super) fn delete(&self, kind: Kind) -> Result<(), StorageFailure> {
        if kind != Kind::Journal {
            return Err(StorageFailure::UnsafeStorage);
        }
        self.verify()?;
        let mut expected = mutex(&self.expected);
        if self.check_locked(kind, &mut expected)?.is_none() {
            return Ok(());
        }
        // SAFETY: only the fixed, checked journal entry is unlinked relative to fd.
        if unsafe { libc::unlinkat(self.fd(), kind.name().as_ptr(), 0) } != 0 {
            return Err(io_failure());
        }
        expected[kind as usize] = None;
        // xDelete must synchronize this namespace change before reporting success.
        Ok(())
    }
}

pub(super) struct Entry {
    file: Option<File>,
    identity: Identity,
    pub(super) kind: Kind,
    pub(super) locks: Mutex<Locks>,
    duplicates: Mutex<Vec<File>>,
}

impl Entry {
    pub(super) fn fd(&self) -> RawFd {
        self.file.as_ref().map_or(-1, AsRawFd::as_raw_fd)
    }

    pub(super) fn verify(&self, directory: &Directory) -> Result<(), StorageFailure> {
        let stat = stat_fd(self.fd())?;
        private_file(&stat)?;
        if Identity::of(&stat) != self.identity
            || directory.check(self.kind)? != Some(self.identity)
        {
            return Err(StorageFailure::UnsafeStorage);
        }
        Ok(())
    }
}

impl Drop for Entry {
    fn drop(&mut self) {
        // Serialize close with inode enrollment. Closing ANY descriptor for an inode
        // releases this process's POSIX locks, even if another descriptor owns them.
        let mut registry = mutex(files());
        registry.remove(&self.identity);
        drop(self.file.take());
        mutex(&self.duplicates).clear();
    }
}
