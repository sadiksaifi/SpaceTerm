use crate::{account_service::ServiceFailure, private_filesystem::Directory};
use sha2::{Digest as _, Sha256};
use spaceterm_distribution::{AccountInstallationManifestV2, VerifiedAccountServerArtifact};
use std::{
    collections::BTreeSet,
    ffi::CString,
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::Path,
};

pub(super) struct Tree(File);

pub(super) struct MutationLock(File);
impl Drop for MutationLock {
    fn drop(&mut self) {
        // Fork shares the open description until exec. Closing our copy alone
        // would let an unrelated child prolong this completed operation's lock.
        // SAFETY: this guard owns the live descriptor on which it acquired flock.
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

impl Tree {
    pub(super) fn open(path: &Path, authority: &Directory) -> Result<Self, ServiceFailure> {
        authority.verify()?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
            .map_err(classify)?;
        let tree = Self(file);
        tree.verify(true)?;
        authority.verify()?;
        let found = std::fs::symlink_metadata(path).map_err(classify)?;
        let retained = tree.0.metadata().map_err(classify)?;
        if found.dev() != retained.dev() || found.ino() != retained.ino() {
            return Err(ServiceFailure::UnsafeStorage);
        }
        Ok(tree)
    }
    fn verify(&self, directory: bool) -> Result<(), ServiceFailure> {
        verify_file(&self.0, directory)
    }
    pub(super) fn identity(&self) -> Result<(u64, u64), ServiceFailure> {
        let metadata = self.0.metadata().map_err(classify)?;
        Ok((metadata.dev(), metadata.ino()))
    }
    pub(super) fn child(&self, name: &str) -> Result<Option<Self>, ServiceFailure> {
        let name = component(name)?;
        // SAFETY: openat receives a retained directory and a single NUL-terminated component.
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            let error = std::io::Error::last_os_error();
            return if error.kind() == std::io::ErrorKind::NotFound {
                Ok(None)
            } else {
                Err(classify(error))
            };
        }
        // SAFETY: successful openat returned a uniquely owned descriptor.
        let tree = Self(unsafe { File::from_raw_fd(fd) });
        tree.verify(true)?;
        Ok(Some(tree))
    }
    pub(super) fn create_child(&self, name: &str) -> Result<Self, ServiceFailure> {
        let value = component(name)?;
        // SAFETY: retained directory and a single component; mode is account-private even under umask 0002.
        if unsafe { libc::mkdirat(self.0.as_raw_fd(), value.as_ptr(), 0o700) } != 0 {
            return Err(classify(std::io::Error::last_os_error()));
        }
        self.child(name)?.ok_or(ServiceFailure::Indeterminate)
    }
    fn parent(&self, name: &str, create: bool) -> Result<(Self, String), ServiceFailure> {
        let (parents, leaf) = name.rsplit_once('/').unwrap_or(("", name));
        component(leaf)?;
        let mut directory = Self(self.0.try_clone().map_err(classify)?);
        for part in parents.split('/').filter(|p| !p.is_empty()) {
            directory = match directory.child(part)? {
                Some(child) => child,
                None if create => directory.create_child(part)?,
                None => return Err(ServiceFailure::Missing),
            };
        }
        Ok((directory, leaf.to_owned()))
    }
    fn create_file(&self, name: &str, mode: u32) -> Result<File, ServiceFailure> {
        let name = component(name)?;
        // SAFETY: exclusive descriptor-relative creation cannot follow or overwrite any entry.
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                mode as libc::c_uint,
            )
        };
        if fd < 0 {
            return Err(classify(std::io::Error::last_os_error()));
        }
        // SAFETY: successful openat returned a uniquely owned descriptor.
        Ok(unsafe { File::from_raw_fd(fd) })
    }
    pub(super) fn read_file(&self, name: &str, mode: u32) -> Result<File, ServiceFailure> {
        let (parent, name) = self.parent(name, false)?;
        let name = component(&name)?;
        // SAFETY: parent remains retained, and the final component cannot be followed.
        let fd = unsafe {
            libc::openat(
                parent.0.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            )
        };
        if fd < 0 {
            return Err(classify(std::io::Error::last_os_error()));
        }
        // SAFETY: successful openat returned a uniquely owned descriptor.
        let file = unsafe { File::from_raw_fd(fd) };
        verify_file(&file, false)?;
        if file.metadata().map_err(classify)?.mode() & 0o7777 != mode {
            return Err(ServiceFailure::UnsafeStorage);
        }
        Ok(file)
    }
    pub(super) fn extract(
        &self,
        artifact: &VerifiedAccountServerArtifact,
    ) -> Result<(), ServiceFailure> {
        let decoder = flate2::read::MultiGzDecoder::new(artifact.bytes());
        let mut archive = tar::Archive::new(decoder);
        for entry in archive
            .entries()
            .map_err(|_| ServiceFailure::ArtifactRejected)?
        {
            let mut entry = entry.map_err(|_| ServiceFailure::ArtifactRejected)?;
            if entry.header().entry_type().is_dir() {
                continue;
            }
            let path = entry.path_bytes();
            let name = std::str::from_utf8(&path)
                .map_err(|_| ServiceFailure::ArtifactRejected)?
                .strip_prefix("spaceterm-server/")
                .ok_or(ServiceFailure::ArtifactRejected)?;
            // The only manifest copy lives in the separately synchronized private metadata record.
            if name == "installation.json" {
                continue;
            }
            let expected = artifact
                .manifest()
                .files
                .iter()
                .find(|f| f.name == name)
                .ok_or(ServiceFailure::ArtifactRejected)?;
            let (parent, leaf) = self.parent(name, true)?;
            let mut file = parent.create_file(&leaf, installed_mode(expected.mode))?;
            #[cfg(feature = "test-support")]
            super::test_support::copying(&mut entry, &mut file)?;
            let copied = std::io::copy(&mut entry, &mut file).map_err(classify)?;
            if copied != expected.size {
                return Err(ServiceFailure::ArtifactRejected);
            }
            file.flush().map_err(classify)?;
            sync(&file)?;
            parent.sync()?;
        }
        // Synchronize every directory, including intermediate parents created without a file directly inside them.
        for name in directories(artifact.manifest()).iter().rev() {
            let (parent, leaf) = self.parent(name, false)?;
            parent
                .child(&leaf)?
                .ok_or(ServiceFailure::Missing)?
                .sync()?;
        }
        self.sync()?;
        self.validate(artifact.manifest())
    }
    pub(super) fn validate(
        &self,
        manifest: &AccountInstallationManifestV2,
    ) -> Result<(), ServiceFailure> {
        self.verify(true)?;
        self.validate_entries(manifest, "")?;
        for expected in &manifest.files {
            let file = self.read_file(&expected.name, installed_mode(expected.mode))?;
            verify_contents(file, expected)?;
        }
        let mut executable = self.read_file(
            &manifest.entry_points.server,
            installed_mode(
                manifest
                    .files
                    .iter()
                    .find(|f| f.name == manifest.entry_points.server)
                    .ok_or(ServiceFailure::ArtifactRejected)?
                    .mode,
            ),
        )?;
        let mut header = [0; 64];
        executable
            .read_exact(&mut header)
            .map_err(|_| ServiceFailure::ArtifactRejected)?;
        let valid = match manifest.platform {
            spaceterm_distribution::ServerPlatform::LinuxX86_64 => {
                header[..7] == *b"\x7fELF\x02\x01\x01"
                    && u16::from_le_bytes([header[18], header[19]]) == 62
                    && matches!(u16::from_le_bytes([header[16], header[17]]), 2 | 3)
            }
            spaceterm_distribution::ServerPlatform::DarwinArm64 => {
                header[..4] == 0xfeedfacf_u32.to_le_bytes()
                    && header[4..8] == 0x0100000c_u32.to_le_bytes()
                    && header[12..16] == 2_u32.to_le_bytes()
            }
        };
        if !valid {
            return Err(ServiceFailure::ArtifactRejected);
        }
        Ok(())
    }
    fn validate_entries(
        &self,
        manifest: &AccountInstallationManifestV2,
        prefix: &str,
    ) -> Result<(), ServiceFailure> {
        use std::ffi::CStr;
        // SAFETY: opening "." below the retained descriptor creates an independent enumeration cursor.
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                c".".as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(ServiceFailure::StorageUnavailable);
        }
        // SAFETY: fdopendir consumes the duplicate on success, not the original descriptor.
        let directory = unsafe { libc::fdopendir(fd) };
        if directory.is_null() {
            // SAFETY: fdopendir failed, so the duplicate is still owned here.
            unsafe { libc::close(fd) };
            return Err(ServiceFailure::StorageUnavailable);
        }
        struct DirectoryStream(*mut libc::DIR);
        impl Drop for DirectoryStream {
            fn drop(&mut self) {
                // SAFETY: this stream exclusively owns its duplicate descriptor.
                unsafe { libc::closedir(self.0) };
            }
        }
        let stream = DirectoryStream(directory);
        let mut count = 0;
        loop {
            // SAFETY: errno is this native thread's error slot.
            unsafe {
                *errno() = 0;
            }
            // SAFETY: the stream is live and used by this thread alone.
            let entry = unsafe { libc::readdir(stream.0) };
            if entry.is_null() {
                // SAFETY: errno is this native thread's error slot.
                return if unsafe { *errno() } == 0 {
                    Ok(())
                } else {
                    Err(ServiceFailure::StorageUnavailable)
                };
            }
            // SAFETY: readdir returned a NUL-terminated name valid until its next call.
            let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }
                .to_str()
                .map_err(|_| ServiceFailure::ArtifactRejected)?;
            if matches!(name, "." | "..") {
                continue;
            }
            count += 1;
            if count > spaceterm_distribution::manifest::MAX_FILES {
                return Err(ServiceFailure::LimitExceeded);
            }
            let full = format!("{prefix}{name}");
            if manifest.files.iter().any(|f| f.name == full) {
                let expected = manifest
                    .files
                    .iter()
                    .find(|f| f.name == full)
                    .ok_or(ServiceFailure::ArtifactRejected)?;
                self.read_file(name, installed_mode(expected.mode))?;
            } else if manifest
                .files
                .iter()
                .any(|f| f.name.starts_with(&format!("{full}/")))
            {
                self.child(name)?
                    .ok_or(ServiceFailure::ArtifactRejected)?
                    .validate_entries(manifest, &format!("{full}/"))?;
            } else {
                return Err(ServiceFailure::ArtifactRejected);
            }
        }
    }
    /// Remove only manifest-named files and their parents. Never traverse unknown entries or links.
    pub(super) fn remove_contents(
        &self,
        manifest: &AccountInstallationManifestV2,
    ) -> Result<(), ServiceFailure> {
        for file in &manifest.files {
            let (parent, leaf) = match self.parent(&file.name, false) {
                Ok(value) => value,
                Err(ServiceFailure::Missing) => continue,
                Err(error) => return Err(error),
            };
            match parent.read_file(&leaf, installed_mode(file.mode)) {
                Ok(retained) => {
                    let identity = retained.metadata().map_err(classify)?;
                    verify_contents(retained, file)?;
                    #[cfg(feature = "test-support")]
                    super::test_support::removing_file();
                    let captured = quarantine_name()?;
                    parent.rename_exclusive(&leaf, &captured)?;
                    let validation = (|| {
                        let retained = parent.read_file(&captured, installed_mode(file.mode))?;
                        let found = retained.metadata().map_err(classify)?;
                        if found.dev() != identity.dev() || found.ino() != identity.ino() {
                            return Err(ServiceFailure::CleanupUnresolved);
                        }
                        // Authenticate the captured bytes, not the pathname inspected before rename.
                        verify_contents(retained, file)
                    })();
                    if let Err(error) = validation {
                        parent.restore(&captured, &leaf)?;
                        return Err(error);
                    }
                    parent.sync()?;
                    parent.unlink(&captured, false)?;
                    parent.sync()?;
                }
                Err(ServiceFailure::Missing) => (),
                Err(error) => return Err(error),
            }
        }
        for name in directories(manifest).iter().rev() {
            let (parent, leaf) = match self.parent(name, false) {
                Ok(value) => value,
                Err(ServiceFailure::Missing) => continue,
                Err(error) => return Err(error),
            };
            if let Some(retained) = parent.child(&leaf)? {
                parent.remove_child(&leaf, &quarantine_name()?, &retained)?;
            }
        }
        self.sync()
    }
    /// Capture before checking identity. Concurrent renames at the original name
    /// cannot turn an inspected entry into a different unlink target.
    pub(super) fn remove_child(
        &self,
        name: &str,
        captured: &str,
        retained: &Self,
    ) -> Result<(), ServiceFailure> {
        self.rename_exclusive(name, captured)?;
        let validation = self.child(captured).and_then(|found| {
            let found = found.ok_or(ServiceFailure::CleanupUnresolved)?;
            if found.identity()? != retained.identity()? {
                return Err(ServiceFailure::CleanupUnresolved);
            }
            Ok(())
        });
        if let Err(error) = validation {
            self.restore(captured, name)?;
            return Err(error);
        }
        self.sync()?;
        if let Err(error) = self.unlink(captured, true) {
            self.restore(captured, name)?;
            return Err(error);
        }
        self.sync()
    }
    fn restore(&self, captured: &str, original: &str) -> Result<(), ServiceFailure> {
        // Never overwrite an entry created after capture. A failed restore keeps
        // both objects and leaves the original cleanup pending.
        self.rename_exclusive(captured, original)?;
        self.sync()
    }
    fn rename_exclusive(&self, from: &str, to: &str) -> Result<(), ServiceFailure> {
        let from = component(from)?;
        let to = component(to)?;
        #[cfg(target_os = "macos")]
        // SAFETY: both names are single components below the retained descriptor.
        let result = unsafe {
            libc::renameatx_np(
                self.0.as_raw_fd(),
                from.as_ptr(),
                self.0.as_raw_fd(),
                to.as_ptr(),
                libc::RENAME_EXCL,
            )
        };
        #[cfg(target_os = "linux")]
        // SAFETY: both names are single components below the retained descriptor.
        let result = unsafe {
            libc::renameat2(
                self.0.as_raw_fd(),
                from.as_ptr(),
                self.0.as_raw_fd(),
                to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if result != 0 {
            return Err(ServiceFailure::CleanupUnresolved);
        }
        Ok(())
    }
    pub(super) fn unlink(&self, name: &str, directory: bool) -> Result<(), ServiceFailure> {
        let name = component(name)?;
        // SAFETY: retained parent and one component. rmdir fails if any unlisted entry remains.
        if unsafe {
            libc::unlinkat(
                self.0.as_raw_fd(),
                name.as_ptr(),
                if directory { libc::AT_REMOVEDIR } else { 0 },
            )
        } != 0
        {
            return Err(ServiceFailure::CleanupUnresolved);
        }
        Ok(())
    }
    pub(super) fn lock(&self) -> Result<MutationLock, ServiceFailure> {
        let file = self.read_file("lock", 0o600)?;
        // SAFETY: flock only changes the advisory lock on this retained descriptor.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(ServiceFailure::Pending);
        }
        Ok(MutationLock(file))
    }
    pub(super) fn sync(&self) -> Result<(), ServiceFailure> {
        self.0.sync_all().map_err(|_| ServiceFailure::Indeterminate)
    }
}

pub(super) fn installed_mode(mode: u32) -> u32 {
    if mode & 0o100 != 0 { 0o700 } else { 0o600 }
}
fn verify_contents(
    file: File,
    expected: &spaceterm_distribution::InstallationFile,
) -> Result<(), ServiceFailure> {
    if file.metadata().map_err(classify)?.len() != expected.size {
        return Err(ServiceFailure::ArtifactRejected);
    }
    let mut file = file.take(expected.size + 1);
    let mut digest = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let n = file.read(&mut buffer).map_err(classify)?;
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
    }
    if <[u8; 32]>::from(digest.finalize()) != expected.sha256 {
        return Err(ServiceFailure::ArtifactRejected);
    }
    Ok(())
}

fn directories(manifest: &AccountInstallationManifestV2) -> BTreeSet<String> {
    let mut result = BTreeSet::new();
    for file in &manifest.files {
        let mut name = file.name.as_str();
        while let Some((parent, _)) = name.rsplit_once('/') {
            result.insert(parent.to_owned());
            name = parent;
        }
    }
    result
}
fn quarantine_name() -> Result<String, ServiceFailure> {
    let mut nonce = [0; 16];
    getrandom::fill(&mut nonce).map_err(|_| ServiceFailure::StorageUnavailable)?;
    let mut name = String::from(".cleanup-");
    use std::fmt::Write as _;
    for byte in nonce {
        write!(name, "{byte:02x}").map_err(|_| ServiceFailure::StorageUnavailable)?;
    }
    Ok(name)
}
fn component(name: &str) -> Result<CString, ServiceFailure> {
    if name.is_empty() || matches!(name, "." | "..") || name.contains('/') {
        return Err(ServiceFailure::UnsafeStorage);
    }
    CString::new(name).map_err(|_| ServiceFailure::UnsafeStorage)
}
fn verify_file(file: &File, directory: bool) -> Result<(), ServiceFailure> {
    let facts = file.metadata().map_err(classify)?;
    // SAFETY: geteuid is a side-effect-free credential query.
    if facts.uid() != unsafe { libc::geteuid() }
        || (directory && (!facts.is_dir() || facts.mode() & 0o7777 != 0o700))
        || (!directory && (!facts.is_file() || facts.nlink() != 1 || facts.mode() & 0o7077 != 0))
    {
        return Err(ServiceFailure::UnsafeStorage);
    }
    #[cfg(target_os = "macos")]
    super::macos::verify_acl(file)?;
    Ok(())
}
#[cfg(target_os = "macos")]
unsafe fn errno() -> *mut libc::c_int {
    // SAFETY: libc provides the current thread's errno slot.
    unsafe { libc::__error() }
}
#[cfg(target_os = "linux")]
unsafe fn errno() -> *mut libc::c_int {
    // SAFETY: libc provides the current thread's errno slot.
    unsafe { libc::__errno_location() }
}
fn sync(file: &File) -> Result<(), ServiceFailure> {
    file.sync_all().map_err(|_| ServiceFailure::Indeterminate)?;
    #[cfg(target_os = "macos")]
    // SAFETY: F_FULLFSYNC synchronizes this retained regular file's storage.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_FULLFSYNC) } != 0 {
        return Err(ServiceFailure::Indeterminate);
    }
    Ok(())
}
fn classify(error: std::io::Error) -> ServiceFailure {
    match error.kind() {
        std::io::ErrorKind::NotFound => ServiceFailure::Missing,
        std::io::ErrorKind::AlreadyExists => ServiceFailure::CleanupUnresolved,
        _ if matches!(error.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR)) => {
            ServiceFailure::UnsafeStorage
        }
        _ => ServiceFailure::StorageUnavailable,
    }
}
