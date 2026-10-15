//! Descriptor-relative private storage. No symlinks, unsafe modes, or foreign owners are repaired.
use crate::ServerError;
use crate::protocol;
use serde::{Deserialize, Serialize};
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

const MAX_RECORD: usize = 4 * 1024 * 1024;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Record {
    pub seed: [u8; 32],
    pub paired: Vec<[u8; 32]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization: Option<crate::organization::Organization>,
}
impl Drop for Record {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.seed.zeroize();
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) struct SocketIdentity {
    device: libc::dev_t,
    inode: libc::ino_t,
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
                if result == 0 {
                    dir.sync_all()
                        .map_err(|_| ServerError::StorageUnavailable)?;
                }
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
    pub fn verify_path(&self, path: &Path) -> Result<(), ServerError> {
        let retained = self
            .0
            .metadata()
            .map_err(|_| ServerError::StorageUnavailable)?;
        let current =
            std::fs::symlink_metadata(path).map_err(|_| ServerError::StorageUnavailable)?;
        if !current.is_dir() || retained.dev() != current.dev() || retained.ino() != current.ino() {
            return Err(ServerError::UnsafeStorage);
        }
        private(&self.0, true)
    }
    pub fn socket_identity(
        &self,
        name: &std::ffi::CStr,
    ) -> Result<Option<SocketIdentity>, ServerError> {
        // SAFETY: zero is valid for every field of stat; fstatat fills the retained output below.
        let mut metadata: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: descriptor, terminated leaf name, and writable output remain live for this call.
        if unsafe {
            libc::fstatat(
                self.0.as_raw_fd(),
                name.as_ptr(),
                &mut metadata,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            return if std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
                Ok(None)
            } else {
                Err(ServerError::StorageUnavailable)
            };
        }
        // SAFETY: geteuid has no preconditions.
        if metadata.st_uid != unsafe { libc::geteuid() }
            || metadata.st_mode & libc::S_IFMT != libc::S_IFSOCK
            || metadata.st_mode & 0o7777 != 0o600
        {
            return Err(ServerError::UnsafeStorage);
        }
        Ok(Some(SocketIdentity {
            device: metadata.st_dev,
            inode: metadata.st_ino,
        }))
    }
    pub fn remove_socket(
        &self,
        name: &std::ffi::CStr,
        expected: SocketIdentity,
    ) -> Result<(), ServerError> {
        // Revalidate immediately before descriptor-relative deletion. Changed entries stay intact.
        if self.socket_identity(name)? != Some(expected) {
            return Err(ServerError::UnsafeStorage);
        }
        // SAFETY: descriptor and terminated single-component name remain live for this call.
        if unsafe { libc::unlinkat(self.0.as_raw_fd(), name.as_ptr(), 0) } != 0 {
            return Err(ServerError::StorageUnavailable);
        }
        self.0
            .sync_all()
            .map_err(|_| ServerError::StorageUnavailable)
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
    /// Validate and synchronize retained state before setup or authentication accepts it.
    pub fn read(&self) -> Result<Record, ServerError> {
        let file = self.file("identity.json", false)?;
        let mut bytes = Zeroizing::new(Vec::new());
        (&file)
            .take((MAX_RECORD + 1) as u64)
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
        if let Some(value) = &record.organization {
            value.validate()?;
        }
        // A previous publication may have renamed successfully but failed to sync its directory.
        // Visible bytes alone do not confirm durability, including after reopening the store.
        file.sync_all()
            .and_then(|()| self.0.sync_all())
            .map_err(|_| ServerError::StorageUnavailable)?;
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
                    organization: None,
                };
                self.write(&record)?;
                Ok(record)
            }
            Err(error) => Err(error),
        }
    }
    /// Caller holds setup.lock. Publish once after fsync so crashes never truncate the old identity.
    pub fn write(&self, record: &Record) -> Result<(), ServerError> {
        if let Some(value) = &record.organization {
            value.validate()?;
        }
        let bytes = Zeroizing::new(
            serde_json::to_vec(record).map_err(|_| ServerError::StorageUnavailable)?,
        );
        self.write_bytes(c"identity.json", &bytes)
    }
    #[cfg(feature = "client")]
    pub(crate) fn read_json<T: serde::de::DeserializeOwned>(
        &self,
        name: &str,
    ) -> Result<Option<T>, ServerError> {
        let file = match self.file(name, false) {
            Ok(file) => file,
            Err(ServerError::NotPaired) => return Ok(None),
            Err(error) => return Err(error),
        };
        let mut bytes = Vec::new();
        (&file)
            .take((MAX_RECORD + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| ServerError::StorageUnavailable)?;
        if bytes.len() > MAX_RECORD {
            return Err(ServerError::UnsafeStorage);
        }
        let value = serde_json::from_slice(&bytes).map_err(|_| ServerError::UnsafeStorage)?;
        file.sync_all()
            .and_then(|()| self.0.sync_all())
            .map_err(|_| ServerError::StorageUnavailable)?;
        Ok(Some(value))
    }
    #[cfg(feature = "client")]
    pub(crate) fn write_json<T: Serialize>(
        &self,
        name: &std::ffi::CStr,
        value: &T,
    ) -> Result<(), ServerError> {
        self.write_bytes(
            name,
            &serde_json::to_vec(value).map_err(|_| ServerError::StorageUnavailable)?,
        )
    }
    fn write_bytes(&self, destination: &std::ffi::CStr, bytes: &[u8]) -> Result<(), ServerError> {
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
            file.write_all(bytes)
                .and_then(|()| file.sync_all())
                .map_err(|_| ServerError::StorageUnavailable)?;
            // SAFETY: both names are single components relative to the same live directory.
            if unsafe {
                libc::renameat(
                    self.0.as_raw_fd(),
                    c_name.as_ptr(),
                    self.0.as_raw_fd(),
                    destination.as_ptr(),
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
    fn organization_is_retained_with_identity_and_pairing() {
        let fixture = Fixture::new();
        let data = fixture.open();
        let _lock = data.lock("setup.lock", false).unwrap();
        let original = data.read_or_create().unwrap();
        let mut value = serde_json::to_value(&original).unwrap();
        value["organization"] = serde_json::json!({
            "version": 1, "workspace": vec![1; 32], "tab": vec![2; 32], "pane": vec![3; 32],
            "name": "Development", "pinned_directory": null,
            "starting_directory": "/tmp", "confirmed_directory": null
        });
        std::fs::write(
            fixture.0.join("identity.json"),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
        let reopened = fixture.open();
        let restored = reopened.read().unwrap();
        assert_eq!(serde_json::to_value(&restored).unwrap(), value);
    }

    #[cfg(feature = "client")]
    #[test]
    fn pending_edits_and_validated_conflict_drafts_survive_client_reopen() {
        use crate::hierarchy::{ClientJournal, Mutation, Outcome, RetainedOutcome};
        let fixture = Fixture::new();
        let mutation = Mutation::RenameWorkspace {
            workspace: [3; 32],
            version: 7,
            name: "Draft".into(),
        };
        let mut journal = ClientJournal::open(fixture.open(), [1; 32], [2; 32]).unwrap();
        journal.submit(4, mutation.clone()).unwrap();
        drop(journal);
        let mut journal = ClientJournal::open(fixture.open(), [1; 32], [2; 32]).unwrap();
        assert_eq!(journal.pending().unwrap().mutation, mutation);
        assert_eq!(journal.pending().unwrap().sequence, 4);
        let result = RetainedOutcome {
            sequence: 4,
            mutation: mutation.clone(),
            outcome: Outcome::Conflict,
        };
        assert_eq!(
            journal.complete(RetainedOutcome {
                sequence: 5,
                ..result.clone()
            }),
            Err(ServerError::Protocol)
        );
        journal.complete(result.clone()).unwrap();
        drop(journal);
        let mut journal = ClientJournal::open(fixture.open(), [1; 32], [2; 32]).unwrap();
        assert_eq!(journal.pending().unwrap().result.as_ref(), Some(&result));
        journal.retire().unwrap();
        drop(journal);
        let journal = ClientJournal::open(fixture.open(), [1; 32], [2; 32]).unwrap();
        assert!(journal.pending().is_none());
        assert_eq!(journal.last(), Some(&result));
        drop(journal);
        assert!(matches!(
            ClientJournal::open(fixture.open(), [9; 32], [2; 32]),
            Err(ServerError::AuthenticationRefused)
        ));
    }

    #[cfg(all(feature = "client", target_os = "linux"))]
    #[test]
    fn uncertain_edit_journal_cannot_resume_until_its_actual_outcome_is_synchronized() {
        use crate::hierarchy::{ClientJournal, Mutation};
        let fixture = Fixture::new();
        let data = fixture.open();
        let mut journal = ClientJournal::open(fixture.open(), [1; 32], [2; 32]).unwrap();
        let fault = sync_failure::fail_directory(&data.0);
        assert_eq!(
            journal.submit(
                1,
                Mutation::CreateWorkspace {
                    name: "Draft".into()
                }
            ),
            Err(ServerError::StorageUnavailable)
        );
        drop(journal);
        assert!(matches!(
            ClientJournal::open(fixture.open(), [1; 32], [2; 32]),
            Err(ServerError::StorageUnavailable)
        ));
        drop(fault);
        let journal = ClientJournal::open(fixture.open(), [1; 32], [2; 32]).unwrap();
        assert_eq!(journal.pending().unwrap().sequence, 1);
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

    #[cfg(target_os = "linux")]
    #[test]
    fn uncertain_pairing_stays_unavailable_until_synchronization_recovers() {
        let fixture = Fixture::new();
        let data = fixture.open();
        let _lock = data.lock("setup.lock", false).unwrap();
        let mut record = data.read_or_create().unwrap();
        record.paired.push([42; 32]);

        let fault = sync_failure::fail_directory(&data.0);
        assert_eq!(data.write(&record), Err(ServerError::StorageUnavailable));
        // The rename succeeded. Recovery must confirm durability, not just parse visible bytes.
        let visible: Record =
            serde_json::from_slice(&fs::read(fixture.0.join("identity.json")).unwrap()).unwrap();
        assert_eq!(visible.paired, [[42; 32]]);
        let reopened = fixture.open();
        assert!(matches!(
            reopened.read(),
            Err(ServerError::StorageUnavailable)
        ));
        assert!(matches!(
            reopened.read_or_create(),
            Err(ServerError::StorageUnavailable)
        ));

        drop(fault);
        let recovered = reopened.read_or_create().unwrap();
        assert_eq!(recovered.seed, record.seed);
        assert_eq!(recovered.paired, [[42; 32]]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn uncertain_organization_publication_blocks_recovery_until_synced() {
        let fixture = Fixture::new();
        let data = fixture.open();
        let _lock = data.lock("setup.lock", false).unwrap();
        let mut record = data.read_or_create().unwrap();
        record.paired.push([42; 32]);
        let mut organization = crate::organization::Organization::first(fixture.0.clone()).unwrap();
        organization.name = "Retained work".into();
        organization.pinned_directory = Some(fixture.0.clone());
        organization.confirmed_directory = Some("/".into());
        let mut shared = crate::hierarchy::State::from_legacy(&organization);
        let creation = crate::hierarchy::Mutation::CreateTab { workspace: organization.workspace };
        let accepted = shared.apply([42; 32], 1, creation.clone(), &fixture.0, |_| true).unwrap();
        organization = shared.envelope(&fixture.0).unwrap();
        record.organization = Some(organization.clone());
        let fault = sync_failure::fail_directory(&data.0);
        assert_eq!(data.write(&record), Err(ServerError::StorageUnavailable));
        let reopened = fixture.open();
        assert!(matches!(
            reopened.read(),
            Err(ServerError::StorageUnavailable)
        ));
        drop(fault);
        let recovered = reopened.read_or_create().unwrap();
        assert_eq!(recovered.seed, record.seed);
        assert_eq!(recovered.paired, [[42; 32]]);
        assert!(recovered.organization.as_ref() == Some(&organization));
        let mut restored = crate::hierarchy::State::from_legacy(recovered.organization.as_ref().unwrap());
        assert_eq!(restored.apply([42; 32], 1, creation, &fixture.0, |_| true).unwrap(), accepted);
        assert_eq!(restored.inventory.workspaces[0].tabs.len(), 2);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_failed_pending_write_preserves_the_previous_organization() {
        let fixture = Fixture::new();
        let data = fixture.open();
        let _lock = data.lock("setup.lock", false).unwrap();
        let mut record = data.read_or_create().unwrap();
        let original = crate::organization::Organization::first(fixture.0.clone()).unwrap();
        record.organization = Some(original.clone());
        data.write(&record).unwrap();
        record.organization.as_mut().unwrap().name = "Uncommitted".into();
        let fault = sync_failure::fail_files();
        assert_eq!(data.write(&record), Err(ServerError::StorageUnavailable));
        assert!(matches!(
            fixture.open().read(),
            Err(ServerError::StorageUnavailable)
        ));
        drop(fault);
        let recovered = fixture.open().read_or_create().unwrap();
        assert!(recovered.organization.as_ref() == Some(&original));
        assert_eq!(recovered.seed, record.seed);
    }

    // Interpose only the native syscall in the Linux test executable. Production storage has
    // no fault switch, and other threads and directories keep using the real filesystem.
    #[cfg(target_os = "linux")]
    mod sync_failure {
        use super::*;
        use std::cell::Cell;

        std::thread_local! {
            static DIRECTORY: Cell<Option<(u64, u64)>> = const { Cell::new(None) };
            static FILES: Cell<bool> = const { Cell::new(false) };
        }

        pub(super) struct Failure;

        pub(super) fn fail_directory(directory: &File) -> Failure {
            let metadata = directory.metadata().unwrap();
            DIRECTORY.set(Some((metadata.dev(), metadata.ino())));
            Failure
        }

        pub(super) fn fail_files() -> Failure {
            FILES.set(true);
            Failure
        }

        impl Drop for Failure {
            fn drop(&mut self) {
                DIRECTORY.set(None);
                FILES.set(false);
            }
        }

        #[unsafe(export_name = "fsync")]
        extern "C" fn fsync_with_failure(fd: libc::c_int) -> libc::c_int {
            // SAFETY: stat is writable and retained throughout fstat.
            let mut metadata: libc::stat = unsafe { std::mem::zeroed() };
            // SAFETY: fstat accepts any descriptor, reporting invalid ones without dereferencing it.
            if unsafe { libc::fstat(fd, &mut metadata) } == 0
                && (DIRECTORY
                    .try_with(|target| target.get() == Some((metadata.st_dev, metadata.st_ino)))
                    .unwrap_or(false)
                    || (metadata.st_mode & libc::S_IFMT == libc::S_IFREG
                        && FILES.try_with(|flag| flag.get()).unwrap_or(false)))
            {
                // SAFETY: __errno_location returns this thread's writable errno pointer.
                unsafe { *libc::__errno_location() = libc::EIO };
                return -1;
            }
            // SAFETY: the fsync syscall takes exactly one descriptor. Bypass this override to
            // preserve the native operation for every other directory and thread.
            unsafe { libc::syscall(libc::SYS_fsync, fd) as libc::c_int }
        }
    }
}
