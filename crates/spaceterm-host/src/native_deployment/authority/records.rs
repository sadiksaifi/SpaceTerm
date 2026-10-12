use std::{
    ffi::{CString, OsStr},
    fs::File,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::MetadataExt,
    },
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use sha2::{Digest, Sha256};

use crate::{
    activation_journal::ActivationJournalRecord,
    native_deployment::{
        bindings,
        contracts::{AccountBuildKey, NativeFailure, validate_key},
        profile::RegistrationProfile,
        publication::NativePublicationProtection,
    },
    native_registration::decode_registration_head,
    protected_directory::{
        NativeDirectoryProtection, OwnedProtectedDirectory, ProtectedDirectoryError,
    },
    startup::StartupSnapshot,
};

const LOCK_WAIT: Duration = Duration::from_secs(2);

pub(super) fn classify(error: ProtectedDirectoryError) -> NativeFailure {
    match error {
        ProtectedDirectoryError::Missing => NativeFailure::RegistrationMissing,
        ProtectedDirectoryError::Unavailable => NativeFailure::StorageUnavailable,
        ProtectedDirectoryError::NativeUnavailable => NativeFailure::CustodyUnavailable,
        ProtectedDirectoryError::TooLarge => NativeFailure::LimitExceeded,
        _ => NativeFailure::UnsafeNativeState,
    }
}
pub(super) fn prefix(key: AccountBuildKey) -> String {
    let build = match key.build_identity {
        crate::startup::BuildIdentity::Release => "release",
        crate::startup::BuildIdentity::Preflight => "preflight",
        crate::startup::BuildIdentity::Development => "development",
    };
    format!("{build}-{}", key.account_uid)
}

pub(super) struct RecordProtection(pub(super) Arc<dyn NativePublicationProtection>);
impl NativeDirectoryProtection for RecordProtection {
    fn verify_directory(&self, file: &File) -> Result<(), ProtectedDirectoryError> {
        self.0.verify_directory(file)?;
        self.0
            .verify_custody(&[file])
            .map_err(|_| ProtectedDirectoryError::Unsafe)
    }
    fn verify_regular_file(&self, file: &File) -> Result<(), ProtectedDirectoryError> {
        self.0.verify_regular_file(file)?;
        self.0
            .verify_custody(&[file])
            .map_err(|_| ProtectedDirectoryError::Unsafe)
    }
}

/// Protected record storage retained independently of the mutable registration head.
/// Provisioning creates stable lock inodes. Reading never creates storage or a replacement identity.
pub struct NativeRegistrationStore {
    pub(super) root: OwnedProtectedDirectory,
    locks: OwnedProtectedDirectory,
    pub(super) path: PathBuf,
    pub(super) protection: Arc<dyn NativePublicationProtection>,
}
impl NativeRegistrationStore {
    /// Opens the fixed production root only behind the reviewed native evidence gate.
    pub fn open_fixed(
        protection: Arc<dyn NativePublicationProtection>,
    ) -> Result<Self, NativeFailure> {
        super::super::production_gate::require_native_evidence()?;
        #[cfg(target_os = "macos")]
        let path = Path::new("/Library/Application Support/SpaceTerm/ServerRegistrations");
        #[cfg(target_os = "linux")]
        let path = Path::new("/var/lib/spaceterm/server-registrations");
        Self::open_native_root(path, protection)
    }
    /// Injection boundary for trusted native composition. A path or DAC inspector alone grants no custody.
    /// The peer must retain fresh publication, alias exclusion, storage, namespaces and mount evidence.
    pub fn open_native_root(
        path: &Path,
        protection: Arc<dyn NativePublicationProtection>,
    ) -> Result<Self, NativeFailure> {
        let inspector = Arc::new(RecordProtection(Arc::clone(&protection)));
        let root = OwnedProtectedDirectory::open(path, inspector.clone()).map_err(classify)?;
        let locks =
            OwnedProtectedDirectory::open(&path.join("locks"), inspector).map_err(classify)?;
        root.verify().map_err(classify)?;
        protection.verify_custody(&[root.file(), locks.file()])?;
        Ok(Self {
            root,
            locks,
            path: path.to_owned(),
            protection,
        })
    }
    pub(super) fn verify(&self) -> Result<(), NativeFailure> {
        self.root.verify().map_err(classify)?;
        self.locks.verify().map_err(classify)?;
        self.protection
            .verify_custody(&[self.root.file(), self.locks.file()])
    }
    pub(super) fn require_clear_progress(&self, key: AccountBuildKey) -> Result<(), NativeFailure> {
        match self.read_journal_document(key) {
            Ok(document) if document.repair.is_some() => return Err(NativeFailure::Indeterminate),
            Ok(_) | Err(NativeFailure::RegistrationMissing) => (),
            Err(error) => return Err(error),
        }
        for suffix in ["head-publication", "head-recovered", "journal-publication"] {
            match self
                .root
                .read(OsStr::new(&format!("{}.{}", prefix(key), suffix)), 4096)
            {
                Err(ProtectedDirectoryError::Missing) => (),
                Ok(_) => return Err(NativeFailure::Indeterminate),
                Err(error) => return Err(classify(error)),
            }
        }
        Ok(())
    }
    /// Reads only the canonical keyed journal, never a greatest-generation scan.
    pub fn read_journal(
        &self,
        key: AccountBuildKey,
    ) -> Result<ActivationJournalRecord, NativeFailure> {
        Ok(self.read_journal_document(key)?.current)
    }
    pub(super) fn read_journal_bytes(
        &self,
        key: AccountBuildKey,
    ) -> Result<Vec<u8>, NativeFailure> {
        validate_key(key)?;
        self.verify()?;
        let bytes = self
            .root
            .read(OsStr::new(&format!("{}.journal", prefix(key))), 64 * 1024)
            .map_err(classify)?;
        self.verify()?;
        Ok(bytes)
    }
    pub(super) fn read_journal_document(
        &self,
        key: AccountBuildKey,
    ) -> Result<super::journal_document::JournalDocument, NativeFailure> {
        let document =
            super::journal_document::JournalDocument::decode(&self.read_journal_bytes(key)?)?;
        if document.current.key != key {
            return Err(NativeFailure::IdentityMismatch);
        }
        Ok(document)
    }
    pub(super) fn journal_bytes_with(
        &self,
        next: &ActivationJournalRecord,
    ) -> Result<Vec<u8>, NativeFailure> {
        let mut document = match self.read_journal_document(next.key) {
            Ok(document) => document,
            Err(NativeFailure::RegistrationMissing) => {
                super::journal_document::JournalDocument::new(next.clone())
            }
            Err(error) => return Err(error),
        };
        document.replace(next)?;
        document.encode()
    }
    /// Reads bounded head/profile/journal comparison facts through retained no-follow ancestors.
    /// It neither issues RetainedRegistration nor authenticates an executable closure.
    pub fn read_snapshot(&self, key: AccountBuildKey) -> Result<StartupSnapshot, NativeFailure> {
        validate_key(key)?;
        self.verify()?;
        let name = prefix(key);
        let bytes = self
            .root
            .read(OsStr::new(&format!("{name}.registration")), 256)
            .map_err(classify)?;
        let head = decode_registration_head(&bytes, key)?;
        let profiles = OwnedProtectedDirectory::open(
            &self.path.join(format!("{name}.profiles")),
            Arc::new(RecordProtection(Arc::clone(&self.protection))),
        )
        .map_err(classify)?;
        self.protection.verify_custody(&[profiles.file()])?;
        let profile = profiles
            .read(
                OsStr::new(&format!("{}.json", bindings::hex(&head.profile_sha256))),
                64 * 1024,
            )
            .map_err(classify)?;
        let digest: [u8; 32] = Sha256::digest(&profile).into();
        if digest != head.profile_sha256 {
            return Err(NativeFailure::InvalidRecord);
        }
        let profile = RegistrationProfile::decode(&profile, key)?;
        if profile.server_id_bytes() != &head.server_id {
            return Err(NativeFailure::IdentityMismatch);
        }
        let journal = match self.read_journal(key) {
            Ok(journal) => Some(journal),
            Err(NativeFailure::RegistrationMissing) if head.active_request.is_none() => None,
            Err(error) => return Err(error),
        };
        profiles.verify().map_err(classify)?;
        self.verify()?;
        // A competing legitimate writer must hold this store's locks. Detect a head changed
        // across this read even when a caller uses the comparison-only API without a lease.
        let current = self
            .root
            .read(OsStr::new(&format!("{name}.registration")), 256)
            .map_err(classify)?;
        if current != bytes {
            return Err(NativeFailure::GenerationMismatch);
        }
        Ok(StartupSnapshot {
            head,
            profile,
            journal,
        })
    }
    pub(super) fn lock(
        &self,
        key: AccountBuildKey,
        deadline: Instant,
    ) -> Result<TransactionLocks, NativeFailure> {
        validate_key(key)?;
        self.verify()?;
        let deadline = deadline.min(Instant::now() + LOCK_WAIT);
        // Every actor opens a fresh descriptor. dup or a descriptor inherited across fork
        // would share an open-file description and cannot serialize independent flock actors.
        let publisher = self.open_lock("publisher.lock", deadline)?;
        let account = self.open_lock(&format!("{}.lock", prefix(key)), deadline)?;
        let locks = TransactionLocks {
            publisher,
            account,
            key,
        };
        self.verify_locks(&locks)?;
        Ok(locks)
    }
    fn open_lock(&self, name: &str, deadline: Instant) -> Result<LockInode, NativeFailure> {
        let name = CString::new(name).map_err(|_| NativeFailure::InvalidRecord)?;
        // SAFETY: retained directory and a canonical single component; no create or follow flags.
        let fd = unsafe {
            libc::openat(
                self.locks.file().as_raw_fd(),
                name.as_ptr(),
                libc::O_RDWR | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            )
        };
        if fd < 0 {
            return Err(NativeFailure::CustodyUnavailable);
        }
        // SAFETY: successful openat returned a newly owned descriptor.
        let file = unsafe { File::from_raw_fd(fd) };
        self.protection
            .verify_regular_file(&file)
            .map_err(classify)?;
        let metadata = file.metadata().map_err(|_| NativeFailure::IoUnavailable)?;
        if !metadata.is_file() || metadata.nlink() != 1 || metadata.len() != 0 {
            return Err(NativeFailure::UnsafeNativeState);
        }
        let lock = LockInode {
            file,
            name,
            device: metadata.dev(),
            inode: metadata.ino(),
        };
        lock.verify(self)?;
        let mut interruptions = 0;
        for _ in 0..2048 {
            if Instant::now() >= deadline {
                return Err(NativeFailure::TimedOut);
            }
            // SAFETY: flock acts on this actor's own retained open-file description.
            if unsafe { libc::flock(lock.file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                lock.verify(self)?;
                return Ok(lock);
            }
            match std::io::Error::last_os_error().raw_os_error() {
                Some(libc::EINTR) if interruptions < 4 => interruptions += 1,
                Some(libc::EWOULDBLOCK) => std::thread::sleep(Duration::from_millis(1)),
                _ => return Err(NativeFailure::IoUnavailable),
            }
        }
        Err(NativeFailure::TimedOut)
    }
    pub(super) fn verify_locks(&self, locks: &TransactionLocks) -> Result<(), NativeFailure> {
        self.verify()?;
        locks.publisher.verify(self)?;
        locks.account.verify(self)
    }
}

pub(crate) struct TransactionLocks {
    publisher: LockInode,
    account: LockInode,
    pub(super) key: AccountBuildKey,
}
struct LockInode {
    file: File,
    name: CString,
    device: u64,
    inode: u64,
}
impl LockInode {
    fn verify(&self, store: &NativeRegistrationStore) -> Result<(), NativeFailure> {
        store
            .protection
            .verify_regular_file(&self.file)
            .map_err(classify)?;
        store.protection.verify_custody(&[&self.file])?;
        let metadata = self
            .file
            .metadata()
            .map_err(|_| NativeFailure::IoUnavailable)?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.len() != 0
            || metadata.dev() != self.device
            || metadata.ino() != self.inode
        {
            return Err(NativeFailure::UnsafeNativeState);
        }
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: the parent is retained, the name is bounded, and stat is read only on success.
        if unsafe {
            libc::fstatat(
                store.locks.file().as_raw_fd(),
                self.name.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            return Err(NativeFailure::UnsafeNativeState);
        }
        // SAFETY: successful fstatat initialized stat.
        let stat = unsafe { stat.assume_init() };
        #[cfg(target_os = "macos")]
        let device = stat.st_dev as u64;
        #[cfg(target_os = "linux")]
        let device = stat.st_dev;
        if device != self.device || stat.st_ino != self.inode {
            return Err(NativeFailure::UnsafeNativeState);
        }
        Ok(())
    }
}
