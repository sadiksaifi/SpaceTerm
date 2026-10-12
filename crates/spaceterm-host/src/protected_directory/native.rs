use std::ffi::{CStr, CString, OsStr};
use std::fs::File;
use std::io::{self, Read};
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use super::observation::{MAX_DIRECTORY_HANDLES, Observation};
use super::{NativeDirectoryProtection, ProtectedDirectoryError};

const MAX_COMPONENT_BYTES: usize = 4096;
const MAX_READ_INTERRUPTS: usize = 16;

pub(super) struct RetainedDirectory {
    root: RetainedHandle,
    children: Vec<RetainedChild>,
}

struct RetainedHandle {
    file: File,
    identity: EntryIdentity,
}

struct RetainedChild {
    name: CString,
    handle: RetainedHandle,
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct EntryIdentity {
    device: libc::dev_t,
    inode: libc::ino_t,
}

struct DirectorySnapshot {
    root: DirectoryVersion,
    children: Vec<DirectoryVersion>,
}

// Directory change time also advances for unrelated children. Compare protection
// attributes here; enrolled inode events detect restored attribute and binding changes.
#[derive(Clone, Copy, Eq, PartialEq)]
struct DirectoryVersion {
    identity: EntryIdentity,
    mode: libc::mode_t,
    owner: libc::uid_t,
    group: libc::gid_t,
}

// Access time is excluded because reading may update it. Modification and change
// times are sampled attributes. Equal values neither rule out intervening edits
// nor authenticate returned contents.
#[derive(Clone, Copy, Eq, PartialEq)]
struct EntryStatus {
    identity: EntryIdentity,
    mode: libc::mode_t,
    owner: libc::uid_t,
    group: libc::gid_t,
    links: libc::nlink_t,
    size: libc::off_t,
    modified_seconds: libc::time_t,
    modified_nanoseconds: libc::c_long,
    changed_seconds: libc::time_t,
    changed_nanoseconds: libc::c_long,
}

impl RetainedDirectory {
    pub(super) fn open(
        absolute_path: &Path,
        protection: &dyn NativeDirectoryProtection,
    ) -> Result<Self, ProtectedDirectoryError> {
        let path = absolute_path.as_os_str().as_bytes();
        // Path::components normalizes interior "." components, so validate the
        // original bytes before traversing any native entry.
        if !path.starts_with(b"/")
            || path.contains(&0)
            || path
                .split(|byte| *byte == b'/')
                .any(|name| name == b"." || name == b"..")
        {
            return Err(ProtectedDirectoryError::InvalidPath);
        }
        let components = path
            .split(|byte| *byte == b'/')
            .filter(|name| !name.is_empty())
            .take(MAX_DIRECTORY_HANDLES)
            .count();
        if components >= MAX_DIRECTORY_HANDLES {
            return Err(ProtectedDirectoryError::Unavailable);
        }
        let mut observation = Observation::begin()?;
        let flags = libc::O_RDONLY
            | libc::O_DIRECTORY
            | libc::O_NOFOLLOW
            | libc::O_CLOEXEC
            | libc::O_NONBLOCK;
        // SAFETY: the fixed root name is NUL-terminated. No creation flags are used.
        let root = owned_file(unsafe { libc::open(c"/".as_ptr(), flags) })?;
        let (root, root_version) = retain_directory(root, protection, &mut observation)?;
        let mut retained = Self {
            root,
            children: Vec::new(),
        };
        let mut snapshot = DirectorySnapshot {
            root: root_version,
            children: Vec::new(),
        };
        for bytes in path
            .split(|byte| *byte == b'/')
            .filter(|name| !name.is_empty())
        {
            let name = component_name(OsStr::from_bytes(bytes))?;
            let file = open_at(&retained.leaf().file, &name, libc::O_DIRECTORY)?;
            let (handle, version) = retain_directory(file, protection, &mut observation)?;
            verify_directory_link(&retained.leaf().file, &name, handle.identity)?;
            retained
                .children
                .try_reserve_exact(1)
                .map_err(|_| ProtectedDirectoryError::Unavailable)?;
            snapshot
                .children
                .try_reserve_exact(1)
                .map_err(|_| ProtectedDirectoryError::Unavailable)?;
            retained.children.push(RetainedChild { name, handle });
            snapshot.children.push(version);
        }
        retained.inspect_protection(protection)?;
        retained.verify_snapshot(&snapshot)?;
        observation.finish()?;
        Ok(retained)
    }

    pub(super) fn verify(
        &self,
        protection: &dyn NativeDirectoryProtection,
    ) -> Result<(), ProtectedDirectoryError> {
        let observation = self.observe()?;
        let snapshot = self.protection_snapshot()?;
        self.inspect_protection(protection)?;
        self.verify_snapshot(&snapshot)?;
        observation.finish()
    }

    fn observe(&self) -> Result<Observation, ProtectedDirectoryError> {
        let mut observation = Observation::begin()?;
        observation.enroll(&self.root.file)?;
        for child in &self.children {
            observation.enroll(&child.handle.file)?;
        }
        Ok(observation)
    }

    fn protection_snapshot(&self) -> Result<DirectorySnapshot, ProtectedDirectoryError> {
        let mut children = Vec::new();
        children
            .try_reserve_exact(self.children.len())
            .map_err(|_| ProtectedDirectoryError::Unavailable)?;
        let root = directory_version(&self.root)?;
        for child in &self.children {
            children.push(directory_version(&child.handle)?);
        }
        Ok(DirectorySnapshot { root, children })
    }

    fn inspect_protection(
        &self,
        protection: &dyn NativeDirectoryProtection,
    ) -> Result<(), ProtectedDirectoryError> {
        verify_directory_handle(&self.root, protection)?;
        for child in &self.children {
            verify_directory_handle(&child.handle, protection)?;
        }
        Ok(())
    }

    fn verify_snapshot(&self, snapshot: &DirectorySnapshot) -> Result<(), ProtectedDirectoryError> {
        // No inspector runs here. This final comparison catches differences still
        // visible after callbacks, not changes restored to equal sampled attributes.
        if directory_version(&self.root)? != snapshot.root {
            return Err(ProtectedDirectoryError::Changed);
        }
        for (child, expected) in self.children.iter().zip(&snapshot.children) {
            if directory_version(&child.handle)? != *expected {
                return Err(ProtectedDirectoryError::Changed);
            }
        }
        self.verify_links()
    }

    fn verify_links(&self) -> Result<(), ProtectedDirectoryError> {
        // Inspect all parent-child links after native protection inspection to
        // detect substitutions still present. Restored bindings can compare equal.
        let mut parent = &self.root;
        for child in &self.children {
            verify_directory_link(&parent.file, &child.name, child.handle.identity)?;
            parent = &child.handle;
        }
        Ok(())
    }

    pub(super) fn read_regular_file(
        &self,
        name: &OsStr,
        maximum_bytes: usize,
        protection: &dyn NativeDirectoryProtection,
    ) -> Result<Vec<u8>, ProtectedDirectoryError> {
        let name = component_name(name)?;
        let read_bound = maximum_bytes
            .checked_add(1)
            .ok_or(ProtectedDirectoryError::TooLarge)?;
        let mut observation = self.observe()?;
        let snapshot = self.protection_snapshot()?;
        self.inspect_protection(protection)?;
        self.verify_snapshot(&snapshot)?;
        let parent = &self.leaf().file;
        let expected = status_at(parent, &name)?;
        expected.require_regular_file()?;
        // Reject special files before opening. O_NONBLOCK also prevents a raced
        // replacement with a FIFO from blocking between status_at and open_at.
        let file = open_at(parent, &name, 0)?;
        observation.enroll(&file)?;
        verify_regular_file(&file, parent, &name, expected, protection)?;
        self.verify_snapshot(&snapshot)?;
        let expected_length =
            usize::try_from(expected.size).map_err(|_| ProtectedDirectoryError::TooLarge)?;
        let read = read_bounded(&file, expected_length, maximum_bytes, read_bound);
        // Even failed reads must not hide a changed identity or protection rule.
        // Keep the initial snapshot through both rounds of inspector callbacks.
        self.inspect_protection(protection)?;
        verify_regular_file(&file, parent, &name, expected, protection)?;
        self.verify_snapshot(&snapshot)?;
        observation.finish()?;
        let bytes = read?;
        if bytes.len() != expected_length {
            return Err(ProtectedDirectoryError::Changed);
        }
        Ok(bytes)
    }

    fn leaf(&self) -> &RetainedHandle {
        self.children
            .last()
            .map_or(&self.root, |child| &child.handle)
    }
}

fn retain_directory(
    file: File,
    protection: &dyn NativeDirectoryProtection,
    observation: &mut Observation,
) -> Result<(RetainedHandle, DirectoryVersion), ProtectedDirectoryError> {
    observation.enroll(&file)?;
    let status = status_of(&file)?;
    status.require_directory()?;
    let handle = RetainedHandle {
        file,
        identity: status.identity,
    };
    let version = verify_directory_handle(&handle, protection)?;
    Ok((handle, version))
}

fn verify_directory_handle(
    handle: &RetainedHandle,
    protection: &dyn NativeDirectoryProtection,
) -> Result<DirectoryVersion, ProtectedDirectoryError> {
    let before = directory_version(handle)?;
    protection.verify_directory(&handle.file)?;
    if directory_version(handle)? != before {
        return Err(ProtectedDirectoryError::Changed);
    }
    Ok(before)
}

fn directory_version(handle: &RetainedHandle) -> Result<DirectoryVersion, ProtectedDirectoryError> {
    let status = status_of(&handle.file)?;
    status.require_directory()?;
    if status.identity != handle.identity {
        return Err(ProtectedDirectoryError::Changed);
    }
    Ok(status.directory_version())
}

fn verify_directory_link(
    parent: &File,
    name: &CStr,
    expected: EntryIdentity,
) -> Result<(), ProtectedDirectoryError> {
    let status = status_at(parent, name).map_err(classify_retained_link)?;
    if status.mode & libc::S_IFMT != libc::S_IFDIR || status.identity != expected {
        return Err(ProtectedDirectoryError::Changed);
    }
    Ok(())
}

fn verify_regular_file(
    file: &File,
    parent: &File,
    name: &CStr,
    expected: EntryStatus,
    protection: &dyn NativeDirectoryProtection,
) -> Result<(), ProtectedDirectoryError> {
    let before = status_of(file)?;
    before.require_regular_file()?;
    if before != expected {
        return Err(ProtectedDirectoryError::Changed);
    }
    protection.verify_regular_file(file)?;
    let after = status_of(file)?;
    after.require_regular_file()?;
    let linked = status_at(parent, name).map_err(classify_retained_link)?;
    if after != expected || linked != expected {
        return Err(ProtectedDirectoryError::Changed);
    }
    Ok(())
}

fn read_bounded(
    file: &File,
    expected_length: usize,
    maximum_bytes: usize,
    read_bound: usize,
) -> Result<Vec<u8>, ProtectedDirectoryError> {
    if expected_length > maximum_bytes {
        return Err(ProtectedDirectoryError::TooLarge);
    }
    let mut reader = file;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(expected_length)
        .map_err(|_| ProtectedDirectoryError::Unavailable)?;
    let mut buffer = [0_u8; 8 * 1024];
    let mut interrupts = 0;
    loop {
        let remaining = read_bound
            .checked_sub(bytes.len())
            .ok_or(ProtectedDirectoryError::TooLarge)?;
        let requested = remaining.min(buffer.len());
        let count = match reader.read(&mut buffer[..requested]) {
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                if interrupts == MAX_READ_INTERRUPTS {
                    return Err(ProtectedDirectoryError::Unavailable);
                }
                interrupts += 1;
                continue;
            }
            Err(error) => return Err(classify(error)),
        };
        if count == 0 {
            return Ok(bytes);
        }
        let length = bytes
            .len()
            .checked_add(count)
            .ok_or(ProtectedDirectoryError::TooLarge)?;
        if length > maximum_bytes {
            return Err(ProtectedDirectoryError::TooLarge);
        }
        bytes
            .try_reserve_exact(count)
            .map_err(|_| ProtectedDirectoryError::Unavailable)?;
        bytes.extend_from_slice(&buffer[..count]);
    }
}

impl EntryStatus {
    fn from_native(status: libc::stat) -> Self {
        let observed = Self {
            identity: EntryIdentity {
                device: status.st_dev,
                inode: status.st_ino,
            },
            mode: status.st_mode,
            owner: status.st_uid,
            group: status.st_gid,
            links: status.st_nlink,
            size: status.st_size,
            modified_seconds: status.st_mtime,
            modified_nanoseconds: status.st_mtime_nsec,
            changed_seconds: status.st_ctime,
            changed_nanoseconds: status.st_ctime_nsec,
        };
        #[cfg(test)]
        let observed = equal_timestamp_regressions::observe_status(observed);
        observed
    }

    fn require_directory(self) -> Result<(), ProtectedDirectoryError> {
        if self.mode & libc::S_IFMT == libc::S_IFDIR {
            Ok(())
        } else {
            Err(ProtectedDirectoryError::Unsafe)
        }
    }

    fn require_regular_file(self) -> Result<(), ProtectedDirectoryError> {
        if self.mode & libc::S_IFMT == libc::S_IFREG && self.links == 1 && self.size >= 0 {
            Ok(())
        } else {
            Err(ProtectedDirectoryError::Unsafe)
        }
    }

    fn directory_version(self) -> DirectoryVersion {
        DirectoryVersion {
            identity: self.identity,
            mode: self.mode,
            owner: self.owner,
            group: self.group,
        }
    }
}

fn component_name(name: &OsStr) -> Result<CString, ProtectedDirectoryError> {
    let bytes = name.as_bytes();
    if bytes.is_empty()
        || bytes.len() > MAX_COMPONENT_BYTES
        || bytes == b"."
        || bytes == b".."
        || bytes.contains(&b'/')
        || bytes.contains(&0)
    {
        return Err(ProtectedDirectoryError::InvalidPath);
    }
    let length = bytes
        .len()
        .checked_add(1)
        .ok_or(ProtectedDirectoryError::InvalidPath)?;
    let mut native_name = Vec::new();
    native_name
        .try_reserve_exact(length)
        .map_err(|_| ProtectedDirectoryError::Unavailable)?;
    native_name.extend_from_slice(bytes);
    native_name.push(0);
    CString::from_vec_with_nul(native_name).map_err(|_| ProtectedDirectoryError::InvalidPath)
}

fn open_at(
    parent: &File,
    name: &CStr,
    additional_flags: libc::c_int,
) -> Result<File, ProtectedDirectoryError> {
    let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK;
    // SAFETY: parent owns a live descriptor and name is NUL-terminated. No creation
    // flags are used, and owned_file takes ownership of any successful descriptor.
    owned_file(unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags | additional_flags) })
}

fn owned_file(descriptor: libc::c_int) -> Result<File, ProtectedDirectoryError> {
    if descriptor < 0 {
        Err(classify(io::Error::last_os_error()))
    } else {
        // SAFETY: the successful open/openat returned a new descriptor owned here.
        Ok(unsafe { File::from_raw_fd(descriptor) })
    }
}

fn status_of(file: &File) -> Result<EntryStatus, ProtectedDirectoryError> {
    let mut status = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: file owns a live descriptor and status is valid writable storage.
    if unsafe { libc::fstat(file.as_raw_fd(), status.as_mut_ptr()) } != 0 {
        return Err(classify(io::Error::last_os_error()));
    }
    // SAFETY: fstat initialized status on success.
    Ok(EntryStatus::from_native(unsafe { status.assume_init() }))
}

fn status_at(parent: &File, name: &CStr) -> Result<EntryStatus, ProtectedDirectoryError> {
    let mut status = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: the live parent descriptor, NUL-terminated name, and writable status
    // remain valid. AT_SYMLINK_NOFOLLOW inspects the link rather than its target.
    let result = unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            status.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result != 0 {
        return Err(classify(io::Error::last_os_error()));
    }
    // SAFETY: fstatat initialized status on success.
    Ok(EntryStatus::from_native(unsafe { status.assume_init() }))
}

fn classify_retained_link(error: ProtectedDirectoryError) -> ProtectedDirectoryError {
    match error {
        ProtectedDirectoryError::Missing | ProtectedDirectoryError::Unsafe => {
            ProtectedDirectoryError::Changed
        }
        other => other,
    }
}

fn classify(error: io::Error) -> ProtectedDirectoryError {
    match error.raw_os_error() {
        Some(
            libc::ELOOP | libc::ENOTDIR | libc::ENXIO | libc::ENODEV | libc::EACCES | libc::EPERM,
        ) => ProtectedDirectoryError::Unsafe,
        Some(libc::ENOENT) => ProtectedDirectoryError::Missing,
        Some(libc::ENAMETOOLONG | libc::EINVAL) => ProtectedDirectoryError::InvalidPath,
        _ => ProtectedDirectoryError::Unavailable,
    }
}

#[cfg(test)]
mod equal_timestamp_regressions {
    use std::cell::{Cell, RefCell};
    use std::ffi::OsStr;
    use std::fs::{self, DirBuilder, File, FileTimes, Metadata, OpenOptions};
    use std::io::{self, Write};
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::EntryStatus;
    use crate::protected_directory::observation::testing::{Fault, FaultScope};
    use crate::protected_directory::{
        NativeDirectoryProtection, ProtectedDirectoryError, RetainedProtectedDirectory,
    };

    const RECORD_NAME: &str = "registration";
    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    std::thread_local! {
        static HOLD_TIMESTAMPS: Cell<bool> = const { Cell::new(false) };
    }

    // Only the native-status observation is normalized. Identities, types, mode,
    // UID/GID, sizes, link counts, inspector metadata and actual files remain real.
    // These tests do not establish native equal-ctime fixtures or mmap coverage.
    pub(super) fn observe_status(mut status: EntryStatus) -> EntryStatus {
        if HOLD_TIMESTAMPS.with(Cell::get) {
            status.modified_seconds = 0;
            status.modified_nanoseconds = 0;
            status.changed_seconds = 0;
            status.changed_nanoseconds = 0;
        }
        status
    }

    struct EqualTimestampObservation;

    impl EqualTimestampObservation {
        fn begin() -> Self {
            HOLD_TIMESTAMPS.with(|held| {
                assert!(!held.replace(true), "timestamp observation is already held");
            });
            Self
        }
    }

    impl Drop for EqualTimestampObservation {
        fn drop(&mut self) {
            HOLD_TIMESTAMPS.with(|held| held.set(false));
        }
    }

    type EntryIdentity = (u64, u64);
    type MetadataWithoutTimes = (u64, u64, u32, u32, u32, u64, u64);

    fn identity(metadata: &Metadata) -> EntryIdentity {
        (metadata.dev(), metadata.ino())
    }

    fn metadata_without_times(metadata: &Metadata) -> MetadataWithoutTimes {
        (
            metadata.dev(),
            metadata.ino(),
            metadata.mode(),
            metadata.uid(),
            metadata.gid(),
            metadata.nlink(),
            metadata.len(),
        )
    }

    // This account-owned fixture inspector is not root-ownership or ACL evidence.
    struct Fixture {
        root: PathBuf,
        ancestor: PathBuf,
        directory: PathBuf,
        controlled_directories: [EntryIdentity; 3],
        owner_uid: u32,
    }

    impl Fixture {
        fn new() -> Self {
            // Alias resolution is fixture setup, never production traversal.
            let parent = fixture_io(std::env::temp_dir().canonicalize());
            let mut root = None;
            for _ in 0..128 {
                let nonce = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
                let candidate = parent.join(format!(
                    "spaceterm-equal-timestamps-{}-{nonce}",
                    std::process::id()
                ));
                match DirBuilder::new().mode(0o700).create(&candidate) {
                    Ok(()) => {
                        root = Some(candidate);
                        break;
                    }
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(_) => panic!("controlled fixture directory creation failed"),
                }
            }
            let root = root.expect("controlled fixture directory allocation failed");
            let ancestor = root.join("ancestor");
            let directory = ancestor.join("registry");
            fixture_io(DirBuilder::new().mode(0o700).create(&ancestor));
            fixture_io(DirBuilder::new().mode(0o700).create(&directory));
            let owner_uid = fixture_io(fs::metadata(&root)).uid();
            let controlled_directories = [&root, &ancestor, &directory]
                .map(|path| identity(&fixture_io(fs::metadata(path))));
            Self {
                root,
                ancestor,
                directory,
                controlled_directories,
                owner_uid,
            }
        }

        fn write_record(&self, bytes: &[u8]) {
            write_private_file(&self.directory.join(RECORD_NAME), bytes);
        }
    }

    impl NativeDirectoryProtection for Fixture {
        fn verify_directory(&self, directory: &File) -> Result<(), ProtectedDirectoryError> {
            let metadata = directory
                .metadata()
                .map_err(|_| ProtectedDirectoryError::Unavailable)?;
            if self.controlled_directories.contains(&identity(&metadata))
                && (metadata.uid() != self.owner_uid || metadata.mode() & 0o777 != 0o700)
            {
                return Err(ProtectedDirectoryError::Unsafe);
            }
            Ok(())
        }

        fn verify_regular_file(&self, file: &File) -> Result<(), ProtectedDirectoryError> {
            let metadata = file
                .metadata()
                .map_err(|_| ProtectedDirectoryError::Unavailable)?;
            if metadata.uid() != self.owner_uid || metadata.mode() & 0o777 != 0o600 {
                return Err(ProtectedDirectoryError::Unsafe);
            }
            Ok(())
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn fixture_io<T>(result: io::Result<T>) -> T {
        result
            .map_err(|_| ())
            .expect("controlled fixture I/O failed")
    }

    fn write_private_file(path: &Path, bytes: &[u8]) {
        let mut file = fixture_io(
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path),
        );
        fixture_io(file.write_all(bytes));
    }

    #[derive(Clone, Copy, Eq, PartialEq)]
    enum Callback {
        FirstArmedDirectory,
        LeafDirectory,
        RegularFile(usize),
    }

    type InspectionAction<'fixture> = dyn FnOnce(&File) + 'fixture;

    struct CallbackProtection<'fixture> {
        fixture: &'fixture Fixture,
        trigger: Callback,
        armed: Cell<bool>,
        file_checks: Cell<usize>,
        action: RefCell<Option<Box<InspectionAction<'fixture>>>>,
    }

    impl<'fixture> CallbackProtection<'fixture> {
        fn new(
            fixture: &'fixture Fixture,
            trigger: Callback,
            action: impl FnOnce(&File) + 'fixture,
        ) -> Self {
            Self {
                fixture,
                trigger,
                armed: Cell::new(false),
                file_checks: Cell::new(0),
                action: RefCell::new(Some(Box::new(action))),
            }
        }

        fn run_action(&self, file: &File) {
            if self.armed.replace(false)
                && let Some(action) = self.action.borrow_mut().take()
            {
                action(file);
            }
        }
    }

    impl NativeDirectoryProtection for CallbackProtection<'_> {
        fn verify_directory(&self, directory: &File) -> Result<(), ProtectedDirectoryError> {
            self.fixture.verify_directory(directory)?;
            let metadata = directory
                .metadata()
                .map_err(|_| ProtectedDirectoryError::Unavailable)?;
            if self.trigger == Callback::FirstArmedDirectory
                || (self.trigger == Callback::LeafDirectory
                    && identity(&metadata) == self.fixture.controlled_directories[2])
            {
                self.run_action(directory);
            }
            Ok(())
        }

        fn verify_regular_file(&self, file: &File) -> Result<(), ProtectedDirectoryError> {
            self.fixture.verify_regular_file(file)?;
            let checks = self.file_checks.get() + 1;
            self.file_checks.set(checks);
            if self.trigger == Callback::RegularFile(checks) {
                self.run_action(file);
            }
            Ok(())
        }
    }

    // All mutation callbacks below complete ordinary filesystem operations before
    // returning. They require bounded observation of those operations, not an
    // atomic filesystem history or detection of stores through old writable maps.
    #[test]
    fn opening_rejects_restored_ancestor_mode_with_equal_observed_timestamps() {
        let fixture = Fixture::new();
        let _timestamps = EqualTimestampObservation::begin();
        let protection = CallbackProtection::new(&fixture, Callback::LeafDirectory, |_| {
            let before = fixture_io(fs::metadata(&fixture.ancestor));
            fixture_io(fs::set_permissions(
                &fixture.ancestor,
                fs::Permissions::from_mode(0o777),
            ));
            fixture_io(fs::set_permissions(
                &fixture.ancestor,
                fs::Permissions::from_mode(0o700),
            ));
            let after = fixture_io(fs::metadata(&fixture.ancestor));
            assert_eq!(
                metadata_without_times(&after),
                metadata_without_times(&before)
            );
        });
        protection.armed.set(true);

        let opened = RetainedProtectedDirectory::open(&fixture.directory, &protection);

        assert!(
            protection.action.borrow().is_none(),
            "the mutation callback did not run"
        );
        assert_eq!(opened.err(), Some(ProtectedDirectoryError::Changed));
    }

    #[test]
    fn verification_rejects_restored_ancestor_mode_with_equal_observed_timestamps() {
        let fixture = Fixture::new();
        let _timestamps = EqualTimestampObservation::begin();
        let protection = CallbackProtection::new(&fixture, Callback::LeafDirectory, |_| {
            let before = fixture_io(fs::metadata(&fixture.ancestor));
            fixture_io(fs::set_permissions(
                &fixture.ancestor,
                fs::Permissions::from_mode(0o777),
            ));
            fixture_io(fs::set_permissions(
                &fixture.ancestor,
                fs::Permissions::from_mode(0o700),
            ));
            let after = fixture_io(fs::metadata(&fixture.ancestor));
            assert_eq!(
                metadata_without_times(&after),
                metadata_without_times(&before)
            );
        });
        let directory = RetainedProtectedDirectory::open(&fixture.directory, &protection)
            .expect("controlled protected directory opening failed");
        protection.armed.set(true);

        let verified = directory.verify();

        assert!(
            protection.action.borrow().is_none(),
            "the mutation callback did not run"
        );
        assert_eq!(verified, Err(ProtectedDirectoryError::Changed));
    }

    #[test]
    fn reading_rejects_restored_ancestor_mode_in_the_final_file_inspector_with_equal_timestamps() {
        let fixture = Fixture::new();
        fixture.write_record(b"record");
        let _timestamps = EqualTimestampObservation::begin();
        let protection = CallbackProtection::new(&fixture, Callback::RegularFile(2), |_| {
            let before = fixture_io(fs::metadata(&fixture.ancestor));
            fixture_io(fs::set_permissions(
                &fixture.ancestor,
                fs::Permissions::from_mode(0o777),
            ));
            fixture_io(fs::set_permissions(
                &fixture.ancestor,
                fs::Permissions::from_mode(0o700),
            ));
            let after = fixture_io(fs::metadata(&fixture.ancestor));
            assert_eq!(
                metadata_without_times(&after),
                metadata_without_times(&before)
            );
        });
        let directory = RetainedProtectedDirectory::open(&fixture.directory, &protection)
            .expect("controlled protected directory opening failed");
        protection.armed.set(true);

        let read = directory.read_regular_file(OsStr::new(RECORD_NAME), 6);

        assert!(
            protection.action.borrow().is_none(),
            "the mutation callback did not run"
        );
        assert_eq!(read.err(), Some(ProtectedDirectoryError::Changed));
    }

    #[test]
    fn verification_rejects_ancestor_substitution_then_original_binding_restoration_with_equal_timestamps()
     {
        let fixture = Fixture::new();
        let _timestamps = EqualTimestampObservation::begin();
        let protection = CallbackProtection::new(&fixture, Callback::LeafDirectory, |_| {
            let before = fixture_io(fs::metadata(&fixture.ancestor));
            let displaced = fixture.root.join("displaced-ancestor");
            fixture_io(fs::rename(&fixture.ancestor, &displaced));
            fixture_io(DirBuilder::new().mode(0o700).create(&fixture.ancestor));
            fixture_io(fs::remove_dir(&fixture.ancestor));
            fixture_io(fs::rename(&displaced, &fixture.ancestor));
            let after = fixture_io(fs::metadata(&fixture.ancestor));
            assert_eq!(
                metadata_without_times(&after),
                metadata_without_times(&before)
            );
        });
        let directory = RetainedProtectedDirectory::open(&fixture.directory, &protection)
            .expect("controlled protected directory opening failed");
        protection.armed.set(true);

        let verified = directory.verify();

        assert!(
            protection.action.borrow().is_none(),
            "the mutation callback did not run"
        );
        assert_eq!(verified, Err(ProtectedDirectoryError::Changed));
    }

    #[test]
    fn reading_rejects_ancestor_substitution_then_original_binding_restoration_during_either_file_inspection()
     {
        for trigger in [Callback::RegularFile(1), Callback::RegularFile(2)] {
            let fixture = Fixture::new();
            fixture.write_record(b"record");
            let _timestamps = EqualTimestampObservation::begin();
            let protection = CallbackProtection::new(&fixture, trigger, |_| {
                let before = fixture_io(fs::metadata(&fixture.ancestor));
                let displaced = fixture.root.join("displaced-ancestor");
                fixture_io(fs::rename(&fixture.ancestor, &displaced));
                fixture_io(DirBuilder::new().mode(0o700).create(&fixture.ancestor));
                fixture_io(fs::remove_dir(&fixture.ancestor));
                fixture_io(fs::rename(&displaced, &fixture.ancestor));
                let after = fixture_io(fs::metadata(&fixture.ancestor));
                assert_eq!(
                    metadata_without_times(&after),
                    metadata_without_times(&before)
                );
            });
            let directory = RetainedProtectedDirectory::open(&fixture.directory, &protection)
                .expect("controlled protected directory opening failed");
            protection.armed.set(true);

            let read = directory.read_regular_file(OsStr::new(RECORD_NAME), 6);

            assert!(
                protection.action.borrow().is_none(),
                "the mutation callback did not run"
            );
            assert_eq!(read.err(), Some(ProtectedDirectoryError::Changed));
        }
    }

    #[test]
    fn reading_rejects_file_substitution_then_original_binding_restoration_during_either_file_inspection()
     {
        for trigger in [Callback::RegularFile(1), Callback::RegularFile(2)] {
            let fixture = Fixture::new();
            fixture.write_record(b"record");
            let _timestamps = EqualTimestampObservation::begin();
            let protection = CallbackProtection::new(&fixture, trigger, |file| {
                let before = fixture_io(file.metadata());
                let path = fixture.directory.join(RECORD_NAME);
                let displaced = fixture.root.join("displaced-record");
                fixture_io(fs::rename(&path, &displaced));
                write_private_file(&path, b"shadow");
                fixture_io(fs::remove_file(&path));
                fixture_io(fs::rename(&displaced, &path));
                let after = fixture_io(fs::metadata(&path));
                assert_eq!(
                    metadata_without_times(&after),
                    metadata_without_times(&before)
                );
            });
            let directory = RetainedProtectedDirectory::open(&fixture.directory, &protection)
                .expect("controlled protected directory opening failed");
            protection.armed.set(true);

            let read = directory.read_regular_file(OsStr::new(RECORD_NAME), 6);

            assert!(
                protection.action.borrow().is_none(),
                "the mutation callback did not run"
            );
            assert_eq!(read.err(), Some(ProtectedDirectoryError::Changed));
        }
    }

    #[test]
    fn reading_rejects_same_length_write_restoring_original_mtime_in_the_first_file_inspector() {
        let fixture = Fixture::new();
        fixture.write_record(b"record");
        let _timestamps = EqualTimestampObservation::begin();
        let protection = CallbackProtection::new(&fixture, Callback::RegularFile(1), |file| {
            let before = fixture_io(file.metadata());
            let original_modified = fixture_io(before.modified());
            let path = fixture.directory.join(RECORD_NAME);
            let mut writer = fixture_io(OpenOptions::new().write(true).open(&path));
            fixture_io(writer.write_all(b"edited"));
            fixture_io(writer.set_times(FileTimes::new().set_modified(original_modified)));
            let after = fixture_io(file.metadata());
            assert_eq!(
                metadata_without_times(&after),
                metadata_without_times(&before)
            );
            assert_eq!(fixture_io(after.modified()), original_modified);
        });
        let directory = RetainedProtectedDirectory::open(&fixture.directory, &protection)
            .expect("controlled protected directory opening failed");
        protection.armed.set(true);

        let read = directory.read_regular_file(OsStr::new(RECORD_NAME), 6);

        assert!(
            protection.action.borrow().is_none(),
            "the mutation callback did not run"
        );
        assert_eq!(read.err(), Some(ProtectedDirectoryError::Changed));
    }

    #[test]
    fn reading_rejects_same_length_write_restoring_original_mtime_in_the_final_file_inspector() {
        let fixture = Fixture::new();
        fixture.write_record(b"record");
        let _timestamps = EqualTimestampObservation::begin();
        let protection = CallbackProtection::new(&fixture, Callback::RegularFile(2), |file| {
            let before = fixture_io(file.metadata());
            let original_modified = fixture_io(before.modified());
            let path = fixture.directory.join(RECORD_NAME);
            let mut writer = fixture_io(OpenOptions::new().write(true).open(&path));
            fixture_io(writer.write_all(b"edited"));
            fixture_io(writer.set_times(FileTimes::new().set_modified(original_modified)));
            let after = fixture_io(file.metadata());
            assert_eq!(
                metadata_without_times(&after),
                metadata_without_times(&before)
            );
            assert_eq!(fixture_io(after.modified()), original_modified);
        });
        let directory = RetainedProtectedDirectory::open(&fixture.directory, &protection)
            .expect("controlled protected directory opening failed");
        protection.armed.set(true);

        let read = directory.read_regular_file(OsStr::new(RECORD_NAME), 6);

        assert!(
            protection.action.borrow().is_none(),
            "the mutation callback did not run"
        );
        assert_eq!(read.err(), Some(ProtectedDirectoryError::Changed));
    }

    #[test]
    fn verification_enrolls_later_ancestors_before_the_first_inspector_runs() {
        let fixture = Fixture::new();
        let _timestamps = EqualTimestampObservation::begin();
        let protection = CallbackProtection::new(&fixture, Callback::FirstArmedDirectory, |_| {
            let before = fixture_io(fs::metadata(&fixture.ancestor));
            fixture_io(fs::set_permissions(
                &fixture.ancestor,
                fs::Permissions::from_mode(0o777),
            ));
            fixture_io(fs::set_permissions(
                &fixture.ancestor,
                fs::Permissions::from_mode(0o700),
            ));
            let after = fixture_io(fs::metadata(&fixture.ancestor));
            assert_eq!(
                metadata_without_times(&after),
                metadata_without_times(&before)
            );
        });
        let directory = RetainedProtectedDirectory::open(&fixture.directory, &protection)
            .expect("controlled protected directory opening failed");
        protection.armed.set(true);

        let verified = directory.verify();

        assert!(
            protection.action.borrow().is_none(),
            "the first inspector did not run"
        );
        assert_eq!(verified, Err(ProtectedDirectoryError::Changed));
    }

    // Synthetic native failures exercise public-operation rejection. They are not
    // evidence of kernel notification coverage, ACL history, or immutable contents.
    #[test]
    fn opening_fails_closed_when_the_native_observation_queue_cannot_be_created() {
        let fixture = Fixture::new();
        let _fault = FaultScope::begin(Fault::QueueCreation);

        let opened = RetainedProtectedDirectory::open(&fixture.directory, &fixture);

        assert_eq!(opened.err(), Some(ProtectedDirectoryError::Unavailable));
    }

    #[test]
    fn verification_fails_closed_when_a_retained_handle_cannot_be_observed() {
        let fixture = Fixture::new();
        let directory = RetainedProtectedDirectory::open(&fixture.directory, &fixture)
            .expect("controlled protected directory opening failed");
        let _fault = FaultScope::begin(Fault::Enrollment);

        let verified = directory.verify();

        assert_eq!(verified, Err(ProtectedDirectoryError::Unavailable));
    }

    #[test]
    fn reading_fails_closed_when_required_native_observation_is_unsupported() {
        let fixture = Fixture::new();
        fixture.write_record(b"record");
        let directory = RetainedProtectedDirectory::open(&fixture.directory, &fixture)
            .expect("controlled protected directory opening failed");
        let _fault = FaultScope::begin(Fault::UnsupportedEnrollment);

        let read = directory.read_regular_file(OsStr::new(RECORD_NAME), 6);

        assert_eq!(read.err(), Some(ProtectedDirectoryError::NativeUnavailable));
    }

    #[test]
    fn reading_enrolls_the_opened_file_before_its_first_protection_inspection() {
        let fixture = Fixture::new();
        fixture.write_record(b"record");
        let fault_scope = RefCell::new(None);
        let protection = CallbackProtection::new(&fixture, Callback::LeafDirectory, |_| {
            *fault_scope.borrow_mut() = Some(FaultScope::begin(Fault::Enrollment));
        });
        let directory = RetainedProtectedDirectory::open(&fixture.directory, &protection)
            .expect("controlled protected directory opening failed");
        protection.armed.set(true);

        let read = directory.read_regular_file(OsStr::new(RECORD_NAME), 6);

        assert!(
            protection.action.borrow().is_none(),
            "the directory inspector did not run"
        );
        assert_eq!(read.err(), Some(ProtectedDirectoryError::Unavailable));
        assert_eq!(protection.file_checks.get(), 0);
    }

    #[test]
    fn reading_rejects_an_unreadable_observation_queue_after_the_final_inspector() {
        let fixture = Fixture::new();
        fixture.write_record(b"record");
        let protection = CallbackProtection::new(&fixture, Callback::RegularFile(2), |_| {});
        let directory = RetainedProtectedDirectory::open(&fixture.directory, &protection)
            .expect("controlled protected directory opening failed");
        protection.armed.set(true);
        let _fault = FaultScope::begin(Fault::QueueRead);

        let read = directory.read_regular_file(OsStr::new(RECORD_NAME), 6);

        assert!(
            protection.action.borrow().is_none(),
            "the final file inspector did not run"
        );
        assert_eq!(protection.file_checks.get(), 2);
        assert_eq!(read.err(), Some(ProtectedDirectoryError::Unavailable));
    }

    #[test]
    fn reading_rejects_watch_loss_even_when_current_protection_and_identity_stay_equal() {
        let fixture = Fixture::new();
        fixture.write_record(b"record");
        let directory = RetainedProtectedDirectory::open(&fixture.directory, &fixture)
            .expect("controlled protected directory opening failed");
        let _fault = FaultScope::begin(Fault::WatchLoss);

        let read = directory.read_regular_file(OsStr::new(RECORD_NAME), 6);

        assert_eq!(read.err(), Some(ProtectedDirectoryError::Changed));
    }

    #[test]
    fn reading_rejects_a_malformed_native_observation_record() {
        let fixture = Fixture::new();
        fixture.write_record(b"record");
        let directory = RetainedProtectedDirectory::open(&fixture.directory, &fixture)
            .expect("controlled protected directory opening failed");
        let _fault = FaultScope::begin(Fault::MalformedRecord);

        let read = directory.read_regular_file(OsStr::new(RECORD_NAME), 6);

        assert_eq!(read.err(), Some(ProtectedDirectoryError::Unavailable));
    }

    #[test]
    fn verification_bounds_an_observation_queue_that_never_finishes_draining() {
        let fixture = Fixture::new();
        let directory = RetainedProtectedDirectory::open(&fixture.directory, &fixture)
            .expect("controlled protected directory opening failed");
        let _fault = FaultScope::begin(Fault::UnboundedDrain);

        let verified = directory.verify();

        assert_eq!(verified, Err(ProtectedDirectoryError::Unavailable));
    }

    #[test]
    fn verification_bounds_repeated_native_observation_interruptions() {
        let fixture = Fixture::new();
        let directory = RetainedProtectedDirectory::open(&fixture.directory, &fixture)
            .expect("controlled protected directory opening failed");
        let _fault = FaultScope::begin(Fault::InterruptedDrain);

        let verified = directory.verify();

        assert_eq!(verified, Err(ProtectedDirectoryError::Unavailable));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn reading_rejects_native_observation_queue_overflow() {
        let fixture = Fixture::new();
        fixture.write_record(b"record");
        let directory = RetainedProtectedDirectory::open(&fixture.directory, &fixture)
            .expect("controlled protected directory opening failed");
        let _fault = FaultScope::begin(Fault::QueueOverflow);

        let read = directory.read_regular_file(OsStr::new(RECORD_NAME), 6);

        assert_eq!(read.err(), Some(ProtectedDirectoryError::Unavailable));
    }

    #[test]
    fn opening_admits_unrelated_child_creation_during_directory_inspection() {
        let fixture = Fixture::new();
        let protection = CallbackProtection::new(&fixture, Callback::LeafDirectory, |_| {
            for parent in [&fixture.root, &fixture.ancestor, &fixture.directory] {
                fixture_io(
                    DirBuilder::new()
                        .mode(0o700)
                        .create(parent.join("unrelated-child")),
                );
            }
        });
        protection.armed.set(true);

        let opened = RetainedProtectedDirectory::open(&fixture.directory, &protection);

        assert!(
            protection.action.borrow().is_none(),
            "the child creation did not run"
        );
        assert!(
            opened.is_ok(),
            "unrelated child creation invalidated opening: {opened:?}"
        );
    }

    #[test]
    fn verification_admits_unrelated_child_creation_during_directory_inspection() {
        let fixture = Fixture::new();
        let protection = CallbackProtection::new(&fixture, Callback::LeafDirectory, |_| {
            for parent in [&fixture.root, &fixture.ancestor, &fixture.directory] {
                fixture_io(
                    DirBuilder::new()
                        .mode(0o700)
                        .create(parent.join("unrelated-child")),
                );
            }
        });
        let directory = RetainedProtectedDirectory::open(&fixture.directory, &protection)
            .expect("controlled protected directory opening failed");
        protection.armed.set(true);

        let verified = directory.verify();

        assert!(
            protection.action.borrow().is_none(),
            "the child creation did not run"
        );
        assert_eq!(verified, Ok(()));
    }

    #[test]
    fn unrelated_child_creation_between_equal_timestamp_operations_remains_admissible() {
        let fixture = Fixture::new();
        fixture.write_record(b"record");
        let _timestamps = EqualTimestampObservation::begin();
        let directory = RetainedProtectedDirectory::open(&fixture.directory, &fixture)
            .expect("controlled protected directory opening failed");
        directory
            .verify()
            .expect("initial protected verification failed");
        let first = directory
            .read_regular_file(OsStr::new(RECORD_NAME), 6)
            .expect("initial protected reading failed");
        assert_eq!(first.as_bytes(), b"record");

        write_private_file(&fixture.directory.join("unrelated-child"), b"other");

        directory
            .verify()
            .expect("between-operation child creation invalidated verification");
        let second = directory
            .read_regular_file(OsStr::new(RECORD_NAME), 6)
            .expect("between-operation child creation invalidated reading");
        // These are ordinary byte-read assertions. Successful observation and
        // returned contents do not authenticate an installation or approve bytes.
        assert_eq!(second.as_bytes(), b"record");
    }
}
