use std::ffi::{CString, OsStr, OsString};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path};
use std::sync::Arc;

use super::secure_filesystem::{
    PreparedPrivateFile, PrivateFileSnapshot, SecureCommitOutcome, SecureDirectory,
    SecureEntryIdentity, SecureFilesystem, SecureFilesystemError,
};

const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
const PRIVATE_FILE_MODE: u32 = 0o600;

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct MacosSecureFilesystem;

#[derive(Debug)]
struct NativeDirectory {
    parent: File,
    file: File,
    name: OsString,
    identity: NativeIdentity,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct NativeIdentity {
    device: u64,
    inode: u64,
}

struct NativePreparedFile {
    directory: Arc<NativeDirectory>,
    file: File,
    identity: NativeIdentity,
    temporary_name: CString,
    target_name: OsString,
    active: bool,
}

impl Drop for NativePreparedFile {
    fn drop(&mut self) {
        if self.active {
            let name = OsStr::from_bytes(self.temporary_name.as_bytes());
            let _ = quarantine_and_remove(
                &self.directory.file,
                name,
                self.identity,
                EntryKind::RegularFile,
            );
        }
    }
}

impl SecureFilesystem for MacosSecureFilesystem {
    fn open_private_directory(
        &self,
        path: &Path,
    ) -> Result<Option<SecureDirectory>, SecureFilesystemError> {
        match open_existing_private_directory(path) {
            Ok(directory) => Ok(Some(wrap_directory(directory))),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(classify(error)),
        }
    }

    fn ensure_private_directory(
        &self,
        path: &Path,
    ) -> Result<SecureDirectory, SecureFilesystemError> {
        ensure_private_directory(path)
            .map(wrap_directory)
            .map_err(classify)
    }

    fn create_private_child(
        &self,
        parent: &SecureDirectory,
        name: &OsStr,
    ) -> Result<SecureDirectory, SecureFilesystemError> {
        let parent = directory(parent)?;
        verify_directory_entry(&parent)?;
        create_directory_at(&parent.file, name).map_err(classify)?;
        let file = open_directory_at(&parent.file, name).map_err(classify)?;
        let identity = metadata_identity(&file.metadata().map_err(classify)?);
        let child_parent = match parent.file.try_clone() {
            Ok(parent) => parent,
            Err(error) => {
                let _ = quarantine_and_remove(&parent.file, name, identity, EntryKind::Directory);
                return Err(classify(error));
            }
        };
        let child = NativeDirectory {
            parent: child_parent,
            file,
            name: name.to_os_string(),
            identity,
        };
        if let Err(error) = set_file_mode(&child.file, PRIVATE_DIRECTORY_MODE)
            .and_then(|()| private_directory_identity(&child.file).map(|_| ()))
        {
            let _ = quarantine_and_remove(&parent.file, name, identity, EntryKind::Directory);
            return Err(classify(error));
        }
        Ok(wrap_directory(child))
    }

    fn verify_directory(
        &self,
        directory_handle: &SecureDirectory,
    ) -> Result<(), SecureFilesystemError> {
        let directory = directory(directory_handle)?;
        verify_directory_entry(&directory)
    }

    fn remove_private_child(
        &self,
        parent_handle: &SecureDirectory,
        name: &OsStr,
        child_handle: &SecureDirectory,
    ) -> Result<(), SecureFilesystemError> {
        let parent = directory(parent_handle)?;
        let child = directory(child_handle)?;
        verify_directory_entry(&parent)?;
        if private_directory_identity(&child.file).map_err(classify)? != child.identity {
            return Err(SecureFilesystemError::Unsafe);
        }
        quarantine_and_remove(&parent.file, name, child.identity, EntryKind::Directory)
    }

    fn read_private_file(
        &self,
        directory_handle: &SecureDirectory,
        name: &OsStr,
        maximum_bytes: usize,
    ) -> Result<Option<PrivateFileSnapshot>, SecureFilesystemError> {
        let directory = directory(directory_handle)?;
        let _transaction = lock_private_directory(&directory)?;
        verify_directory_entry(&directory)?;
        let file = match open_file_at(&directory.file, name, libc::O_RDONLY | libc::O_NONBLOCK, 0) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(classify(error)),
        };
        let identity = private_file_identity(&file.metadata().map_err(classify)?)?;
        let mut bytes = Vec::new();
        (&file)
            .take(maximum_bytes.saturating_add(1) as u64)
            .read_to_end(&mut bytes)
            .map_err(classify)?;
        if bytes.len() > maximum_bytes {
            return Err(SecureFilesystemError::Unsafe);
        }
        verify_directory_entry(&directory)?;
        let current =
            file_identity_at(&directory.file, name)?.ok_or(SecureFilesystemError::Unsafe)?;
        if current != identity {
            return Err(SecureFilesystemError::Unsafe);
        }
        Ok(Some(PrivateFileSnapshot {
            bytes,
            identity: SecureEntryIdentity(Arc::new(identity)),
        }))
    }

    fn prepare_private_file(
        &self,
        directory_handle: &SecureDirectory,
        target: &OsStr,
        bytes: &[u8],
        allocation_nonce: [u8; 16],
    ) -> Result<PreparedPrivateFile, SecureFilesystemError> {
        let directory = directory(directory_handle)?;
        verify_directory_entry(&directory)?;
        let temporary_name = temporary_name(target, allocation_nonce)?;
        let temporary = open_file_at_cstring(
            &directory.file,
            &temporary_name,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
            PRIVATE_FILE_MODE,
        )
        .map_err(classify)?;
        let identity = metadata_identity(&temporary.metadata().map_err(classify)?);
        let mut prepared = NativePreparedFile {
            directory: Arc::clone(&directory),
            file: temporary,
            identity,
            temporary_name,
            target_name: target.to_os_string(),
            active: true,
        };
        if let Err(error) = (|| {
            set_file_mode(&prepared.file, PRIVATE_FILE_MODE)?;
            private_file_identity(&prepared.file.metadata()?).map_err(as_io_error)?;
            (&prepared.file).write_all(bytes)?;
            prepared.file.sync_all()?;
            verify_directory_entry(&directory).map_err(as_io_error)
        })() {
            prepared.active = true;
            return Err(classify(error));
        }
        Ok(PreparedPrivateFile(Box::new(prepared)))
    }

    fn commit_private_file(
        &self,
        prepared: PreparedPrivateFile,
        expected: Option<&SecureEntryIdentity>,
    ) -> Result<SecureCommitOutcome, SecureFilesystemError> {
        let mut prepared = prepared
            .0
            .downcast::<NativePreparedFile>()
            .map_err(|_| SecureFilesystemError::Unsafe)?;
        let _transaction = lock_private_directory(&prepared.directory)?;
        verify_directory_entry(&prepared.directory)?;
        let open_identity = private_file_identity(&prepared.file.metadata().map_err(classify)?)?;
        if open_identity != prepared.identity {
            return Err(SecureFilesystemError::Unsafe);
        }
        let temporary_name = OsStr::from_bytes(prepared.temporary_name.as_bytes());
        if file_identity_at(&prepared.directory.file, temporary_name)? != Some(prepared.identity) {
            return Err(SecureFilesystemError::Unsafe);
        }
        let actual = file_identity_at(&prepared.directory.file, &prepared.target_name)?;
        let expected = expected.map(identity).transpose()?.copied();
        if actual != expected {
            return Ok(SecureCommitOutcome::Conflict);
        }
        if let Some(expected) = expected {
            validate_prepared_file(&prepared)?;
            run_before_private_file_publish_hook();
            swap_at(
                &prepared.directory.file,
                &prepared.temporary_name,
                &prepared.target_name,
            )
            .map_err(classify)?;

            run_after_private_file_publish_hook();

            if let Err(error) = validate_prepared_file(&prepared) {
                rollback_prepared_swap(&mut prepared)?;
                return Err(error);
            }
            match file_identity_at(&prepared.directory.file, &prepared.target_name) {
                Ok(Some(installed)) if installed == prepared.identity => {}
                Ok(_) => {
                    rollback_prepared_swap(&mut prepared)?;
                    return Err(SecureFilesystemError::Unsafe);
                }
                Err(error) => {
                    rollback_prepared_swap(&mut prepared)?;
                    return Err(error);
                }
            }
            let displaced_name = OsStr::from_bytes(prepared.temporary_name.as_bytes());
            match file_identity_at(&prepared.directory.file, displaced_name) {
                Ok(Some(displaced)) if displaced == expected => {}
                Ok(_) => {
                    rollback_prepared_swap(&mut prepared)?;
                    return Ok(SecureCommitOutcome::Conflict);
                }
                Err(error) => {
                    rollback_prepared_swap(&mut prepared)?;
                    return Err(error);
                }
            }
            if let Err(error) = verify_directory_entry(&prepared.directory) {
                rollback_prepared_swap(&mut prepared)?;
                return Err(error);
            }
            if let Err(error) = quarantine_and_remove(
                &prepared.directory.file,
                displaced_name,
                expected,
                EntryKind::RegularFile,
            ) {
                rollback_prepared_swap(&mut prepared)?;
                return Err(error);
            }
        } else {
            validate_prepared_file(&prepared)?;
            let target_name = component_cstring(&prepared.target_name).map_err(classify)?;
            run_before_private_file_publish_hook();
            match rename_exclusive_at(&prepared.directory.file, temporary_name, &target_name) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    return Ok(SecureCommitOutcome::Conflict);
                }
                Err(error) => return Err(classify(error)),
            }
            run_after_private_file_publish_hook();
            if let Err(error) = validate_prepared_file(&prepared) {
                rollback_exclusive_publish(&mut prepared)?;
                return Err(error);
            }
            match file_identity_at(&prepared.directory.file, &prepared.target_name) {
                Ok(Some(installed)) if installed == prepared.identity => {}
                Ok(_) => {
                    rollback_exclusive_publish(&mut prepared)?;
                    return Err(SecureFilesystemError::Unsafe);
                }
                Err(error) => {
                    rollback_exclusive_publish(&mut prepared)?;
                    return Err(error);
                }
            }
            if let Err(error) = verify_directory_entry(&prepared.directory) {
                rollback_exclusive_publish(&mut prepared)?;
                return Err(error);
            }
        }
        prepared.active = false;
        match prepared.directory.file.sync_all() {
            Ok(()) => Ok(SecureCommitOutcome::Committed),
            Err(_) => Ok(SecureCommitOutcome::CommittedButUnsynced),
        }
    }

    fn register_socket(
        &self,
        directory_handle: &SecureDirectory,
        name: &OsStr,
    ) -> Result<SecureEntryIdentity, SecureFilesystemError> {
        let directory = directory(directory_handle)?;
        verify_directory_entry(&directory)?;
        let before = socket_identity_at(&directory.file, name, None)?;
        set_entry_mode_at(&directory.file, name, PRIVATE_FILE_MODE).map_err(classify)?;
        let after = socket_identity_at(&directory.file, name, Some(PRIVATE_FILE_MODE))?;
        if before != after {
            return Err(SecureFilesystemError::Unsafe);
        }
        verify_directory_entry(&directory)?;
        Ok(SecureEntryIdentity(Arc::new(after)))
    }

    fn verify_socket(
        &self,
        directory_handle: &SecureDirectory,
        name: &OsStr,
        identity_handle: &SecureEntryIdentity,
    ) -> Result<(), SecureFilesystemError> {
        let directory = directory(directory_handle)?;
        verify_directory_entry(&directory)?;
        let actual = socket_identity_at(&directory.file, name, Some(PRIVATE_FILE_MODE))?;
        if &actual == identity(identity_handle)? {
            Ok(())
        } else {
            Err(SecureFilesystemError::Unsafe)
        }
    }

    fn remove_socket(
        &self,
        directory_handle: &SecureDirectory,
        name: &OsStr,
        identity_handle: &SecureEntryIdentity,
    ) -> Result<(), SecureFilesystemError> {
        let directory = directory(directory_handle)?;
        verify_directory_entry(&directory)?;
        quarantine_and_remove(
            &directory.file,
            name,
            *identity(identity_handle)?,
            EntryKind::Socket,
        )
    }

    #[cfg(test)]
    fn create_private_artifact(
        &self,
        directory_handle: &SecureDirectory,
        name: &OsStr,
    ) -> Result<(), SecureFilesystemError> {
        let directory = directory(directory_handle)?;
        verify_directory_entry(&directory)?;
        let file = open_file_at(
            &directory.file,
            name,
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL,
            PRIVATE_FILE_MODE,
        )
        .map_err(classify)?;
        set_file_mode(&file, PRIVATE_FILE_MODE).map_err(classify)?;
        private_file_identity(&file.metadata().map_err(classify)?)?;
        Ok(())
    }
}

fn wrap_directory(directory: NativeDirectory) -> SecureDirectory {
    SecureDirectory(Arc::new(directory))
}

fn directory(handle: &SecureDirectory) -> Result<Arc<NativeDirectory>, SecureFilesystemError> {
    Arc::clone(&handle.0)
        .downcast::<NativeDirectory>()
        .map_err(|_| SecureFilesystemError::Unsafe)
}

fn identity(handle: &SecureEntryIdentity) -> Result<&NativeIdentity, SecureFilesystemError> {
    handle
        .0
        .downcast_ref::<NativeIdentity>()
        .ok_or(SecureFilesystemError::Unsafe)
}

fn classify(error: io::Error) -> SecureFilesystemError {
    if matches!(error.raw_os_error(), Some(code) if code == libc::ELOOP || code == libc::ENOTDIR) {
        return SecureFilesystemError::Unsafe;
    }
    match error.kind() {
        io::ErrorKind::NotFound => SecureFilesystemError::Missing,
        io::ErrorKind::AlreadyExists => SecureFilesystemError::AlreadyExists,
        io::ErrorKind::InvalidInput | io::ErrorKind::PermissionDenied => {
            SecureFilesystemError::Unsafe
        }
        _ => SecureFilesystemError::Unavailable,
    }
}

fn as_io_error(error: SecureFilesystemError) -> io::Error {
    let kind = match error {
        SecureFilesystemError::Missing => io::ErrorKind::NotFound,
        SecureFilesystemError::AlreadyExists => io::ErrorKind::AlreadyExists,
        SecureFilesystemError::Unsafe => io::ErrorKind::PermissionDenied,
        SecureFilesystemError::Unavailable => io::ErrorKind::Other,
    };
    io::Error::from(kind)
}

fn ensure_private_directory(path: &Path) -> io::Result<NativeDirectory> {
    if !path.is_absolute() {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    let mut parent = File::open("/")?;
    let mut components = path.components().peekable();
    let mut rollback = DirectoryRollback::default();
    while let Some(component) = components.next() {
        let Component::Normal(name) = component else {
            if matches!(component, Component::RootDir) {
                continue;
            }
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        };
        let is_final = components.peek().is_none();
        let (directory, was_created) = match open_directory_at(&parent, name) {
            Ok(directory) => (directory, false),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match create_directory_at(&parent, name) {
                    Ok(()) => {
                        let directory = open_directory_at(&parent, name)?;
                        let identity = metadata_identity(&directory.metadata()?);
                        if let Err(error) = rollback.record(&parent, name, identity) {
                            let _ = quarantine_and_remove(
                                &parent,
                                name,
                                identity,
                                EntryKind::Directory,
                            );
                            return Err(error);
                        }
                        (directory, true)
                    }
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        (open_directory_at(&parent, name)?, false)
                    }
                    Err(error) => {
                        return Err(error);
                    }
                }
            }
            Err(error) => {
                return Err(error);
            }
        };
        if was_created {
            set_file_mode(&directory, PRIVATE_DIRECTORY_MODE)?;
        }
        if is_final {
            if !was_created && directory.metadata()?.uid() != effective_user_id() {
                return Err(io::Error::from(io::ErrorKind::PermissionDenied));
            }
            if !was_created {
                set_file_mode(&directory, PRIVATE_DIRECTORY_MODE)?;
            }
            let identity = private_directory_identity(&directory)?;
            let parent_clone = parent.try_clone()?;
            rollback.disarm();
            return Ok(NativeDirectory {
                parent: parent_clone,
                file: directory,
                name: name.to_os_string(),
                identity,
            });
        }
        parent = directory;
    }
    Err(io::Error::from(io::ErrorKind::InvalidInput))
}

fn open_existing_private_directory(path: &Path) -> io::Result<NativeDirectory> {
    if !path.is_absolute() {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    let mut parent = File::open("/")?;
    let mut components = path.components().peekable();
    while let Some(component) = components.next() {
        let Component::Normal(name) = component else {
            if matches!(component, Component::RootDir) {
                continue;
            }
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        };
        let directory = open_directory_at(&parent, name)?;
        if components.peek().is_none() {
            let identity = private_directory_identity(&directory)?;
            return Ok(NativeDirectory {
                parent,
                file: directory,
                name: name.to_os_string(),
                identity,
            });
        }
        parent = directory;
    }
    Err(io::Error::from(io::ErrorKind::InvalidInput))
}

fn verify_directory_entry(directory: &NativeDirectory) -> Result<(), SecureFilesystemError> {
    let entry = open_directory_at(&directory.parent, &directory.name).map_err(classify)?;
    let entry_identity = private_directory_identity(&entry).map_err(classify)?;
    let open_identity = private_directory_identity(&directory.file).map_err(classify)?;
    if entry_identity == directory.identity && open_identity == directory.identity {
        Ok(())
    } else {
        Err(SecureFilesystemError::Unsafe)
    }
}

fn lock_private_directory(directory: &NativeDirectory) -> Result<File, SecureFilesystemError> {
    // A separate open description coordinates clones, independent adapters, and other processes.
    // The directory identity survives replacement of the configuration file it protects.
    let lock = open_directory_at(&directory.parent, &directory.name).map_err(classify)?;
    if private_directory_identity(&lock).map_err(classify)? != directory.identity {
        return Err(SecureFilesystemError::Unsafe);
    }
    lock.lock().map_err(classify)?;
    verify_directory_entry(directory)?;
    Ok(lock)
}

fn private_directory_identity(file: &File) -> io::Result<NativeIdentity> {
    let metadata = file.metadata()?;
    if !metadata.is_dir()
        || metadata.uid() != effective_user_id()
        || metadata.mode() & 0o7777 != PRIVATE_DIRECTORY_MODE
    {
        return Err(io::Error::from(io::ErrorKind::PermissionDenied));
    }
    Ok(metadata_identity(&metadata))
}

fn private_file_identity(metadata: &fs::Metadata) -> Result<NativeIdentity, SecureFilesystemError> {
    if !metadata.is_file()
        || metadata.uid() != effective_user_id()
        || metadata.mode() & 0o7777 != PRIVATE_FILE_MODE
        || metadata.nlink() != 1
    {
        return Err(SecureFilesystemError::Unsafe);
    }
    Ok(metadata_identity(metadata))
}

fn validate_prepared_file(prepared: &NativePreparedFile) -> Result<(), SecureFilesystemError> {
    let open_identity = private_file_identity(&prepared.file.metadata().map_err(classify)?)?;
    if open_identity != prepared.identity {
        return Err(SecureFilesystemError::Unsafe);
    }
    Ok(())
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_PRIVATE_FILE_PUBLISH_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        std::cell::RefCell::new(None);
    static AFTER_PRIVATE_FILE_PUBLISH_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        std::cell::RefCell::new(None);
}

#[cfg(test)]
fn install_before_private_file_publish_hook(hook: impl FnOnce() + 'static) {
    BEFORE_PRIVATE_FILE_PUBLISH_HOOK.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(hook));
    });
}

#[cfg(test)]
fn run_before_private_file_publish_hook() {
    BEFORE_PRIVATE_FILE_PUBLISH_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn run_before_private_file_publish_hook() {}

#[cfg(test)]
fn run_after_private_file_publish_hook() {
    AFTER_PRIVATE_FILE_PUBLISH_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn run_after_private_file_publish_hook() {}

fn rollback_prepared_swap(prepared: &mut NativePreparedFile) -> Result<(), SecureFilesystemError> {
    prepared.active = false;
    let withdrawn = withdraw_prepared_file(prepared)?;
    let target = component_cstring(&prepared.target_name).map_err(classify)?;
    if rename_exclusive_at(
        &prepared.directory.file,
        OsStr::from_bytes(prepared.temporary_name.as_bytes()),
        &target,
    )
    .is_err()
    {
        let _ = rename_exclusive_at(
            &prepared.directory.file,
            OsStr::from_bytes(withdrawn.as_bytes()),
            &target,
        );
        return Err(SecureFilesystemError::Unavailable);
    }
    quarantine_and_remove_retained_file(
        &prepared.directory.file,
        OsStr::from_bytes(withdrawn.as_bytes()),
        prepared.identity,
    )
    .map_err(|_| SecureFilesystemError::Unavailable)
}

fn rollback_exclusive_publish(
    prepared: &mut NativePreparedFile,
) -> Result<(), SecureFilesystemError> {
    prepared.active = false;
    let withdrawn = withdraw_prepared_file(prepared)?;
    quarantine_and_remove_retained_file(
        &prepared.directory.file,
        OsStr::from_bytes(withdrawn.as_bytes()),
        prepared.identity,
    )
    .map_err(|_| SecureFilesystemError::Unavailable)
}

fn withdraw_prepared_file(prepared: &NativePreparedFile) -> Result<CString, SecureFilesystemError> {
    let parent = &prepared.directory.file;
    let withdrawn = quarantine_name(&prepared.target_name)?;
    rename_exclusive_at(parent, &prepared.target_name, &withdrawn).map_err(classify)?;
    let withdrawn_name = OsStr::from_bytes(withdrawn.as_bytes());
    if retained_private_file_identity_at(parent, withdrawn_name) == Ok(prepared.identity) {
        return Ok(withdrawn);
    }
    // A writer ignoring the transaction lock can replace the publication. Restore that exact
    // entry without overwriting anything newer, and retain the predecessor for recovery.
    let target = component_cstring(&prepared.target_name).map_err(classify)?;
    rename_exclusive_at(parent, withdrawn_name, &target).map_err(classify)?;
    Err(SecureFilesystemError::Unsafe)
}

fn metadata_identity(metadata: &fs::Metadata) -> NativeIdentity {
    NativeIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    }
}

#[derive(Clone, Copy)]
enum EntryKind {
    RegularFile,
    Socket,
    Directory,
}

fn entry_identity_at(
    parent: &File,
    name: &OsStr,
    kind: EntryKind,
) -> Result<NativeIdentity, SecureFilesystemError> {
    let status = status_at(parent, name).map_err(classify)?;
    let mode = u32::from(status.st_mode);
    let is_private_entry = match kind {
        EntryKind::RegularFile => {
            mode & u32::from(libc::S_IFMT) == u32::from(libc::S_IFREG)
                && status.st_uid == effective_user_id()
                && mode & 0o7777 == PRIVATE_FILE_MODE
                && status.st_nlink == 1
        }
        EntryKind::Socket => {
            mode & u32::from(libc::S_IFMT) == u32::from(libc::S_IFSOCK)
                && status.st_uid == effective_user_id()
                && mode & 0o777 == PRIVATE_FILE_MODE
                && status.st_nlink == 1
        }
        EntryKind::Directory => {
            mode & u32::from(libc::S_IFMT) == u32::from(libc::S_IFDIR)
                && status.st_uid == effective_user_id()
                && mode & 0o7777 == PRIVATE_DIRECTORY_MODE
        }
    };
    if !is_private_entry {
        return Err(SecureFilesystemError::Unsafe);
    }
    Ok(NativeIdentity {
        device: status.st_dev as u64,
        inode: status.st_ino,
    })
}

fn file_identity_at(
    parent: &File,
    name: &OsStr,
) -> Result<Option<NativeIdentity>, SecureFilesystemError> {
    let status = match status_at(parent, name) {
        Ok(status) => status,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(classify(error)),
    };
    let mode = u32::from(status.st_mode);
    if mode & u32::from(libc::S_IFMT) != u32::from(libc::S_IFREG)
        || status.st_uid != effective_user_id()
        || mode & 0o7777 != PRIVATE_FILE_MODE
        || status.st_nlink != 1
    {
        return Err(SecureFilesystemError::Unsafe);
    }
    Ok(Some(NativeIdentity {
        device: status.st_dev as u64,
        inode: status.st_ino,
    }))
}

fn socket_identity_at(
    parent: &File,
    name: &OsStr,
    expected_mode: Option<u32>,
) -> Result<NativeIdentity, SecureFilesystemError> {
    let status = status_at(parent, name).map_err(classify)?;
    let mode = u32::from(status.st_mode);
    if mode & u32::from(libc::S_IFMT) != u32::from(libc::S_IFSOCK)
        || status.st_uid != effective_user_id()
        || expected_mode.is_some_and(|expected| mode & 0o777 != expected)
        || status.st_nlink != 1
    {
        return Err(SecureFilesystemError::Unsafe);
    }
    Ok(NativeIdentity {
        device: status.st_dev as u64,
        inode: status.st_ino,
    })
}

fn quarantine_and_remove(
    parent: &File,
    name: &OsStr,
    expected: NativeIdentity,
    kind: EntryKind,
) -> Result<(), SecureFilesystemError> {
    let quarantine = quarantine_name(name)?;
    let original = component_cstring(name).map_err(classify)?;
    rename_exclusive_at(parent, name, &quarantine).map_err(classify)?;
    let quarantine_name = OsStr::from_bytes(quarantine.as_bytes());
    let observed = entry_identity_at(parent, quarantine_name, kind);
    match observed {
        Ok(observed) if observed == expected => {
            let flags = if matches!(kind, EntryKind::Directory) {
                libc::AT_REMOVEDIR
            } else {
                0
            };
            if let Err(error) = remove_at(parent, quarantine_name, flags) {
                let _ = rename_exclusive_at(parent, quarantine_name, &original);
                return Err(classify(error));
            }
            Ok(())
        }
        Ok(_) | Err(SecureFilesystemError::Unsafe) => {
            rename_exclusive_at(parent, quarantine_name, &original).map_err(classify)?;
            Err(SecureFilesystemError::Unsafe)
        }
        Err(error) => {
            let _ = rename_exclusive_at(parent, quarantine_name, &original);
            Err(error)
        }
    }
}

fn quarantine_and_remove_retained_file(
    parent: &File,
    name: &OsStr,
    expected: NativeIdentity,
) -> Result<(), SecureFilesystemError> {
    let quarantine = quarantine_name(name)?;
    let original = component_cstring(name).map_err(classify)?;
    rename_exclusive_at(parent, name, &quarantine).map_err(classify)?;
    let quarantine_name = OsStr::from_bytes(quarantine.as_bytes());
    let observed = retained_private_file_identity_at(parent, quarantine_name);
    match observed {
        Ok(observed) if observed == expected => {
            if let Err(error) = remove_at(parent, quarantine_name, 0) {
                let _ = rename_exclusive_at(parent, quarantine_name, &original);
                return Err(classify(error));
            }
            Ok(())
        }
        Ok(_) | Err(SecureFilesystemError::Unsafe) => {
            rename_exclusive_at(parent, quarantine_name, &original).map_err(classify)?;
            Err(SecureFilesystemError::Unsafe)
        }
        Err(error) => {
            let _ = rename_exclusive_at(parent, quarantine_name, &original);
            Err(error)
        }
    }
}

fn retained_private_file_identity_at(
    parent: &File,
    name: &OsStr,
) -> Result<NativeIdentity, SecureFilesystemError> {
    let status = status_at(parent, name).map_err(classify)?;
    let mode = u32::from(status.st_mode);
    if mode & u32::from(libc::S_IFMT) != u32::from(libc::S_IFREG)
        || status.st_uid != effective_user_id()
        || mode & 0o7777 != PRIVATE_FILE_MODE
        || status.st_nlink < 1
    {
        return Err(SecureFilesystemError::Unsafe);
    }
    Ok(NativeIdentity {
        device: status.st_dev as u64,
        inode: status.st_ino,
    })
}

fn status_at(parent: &File, name: &OsStr) -> io::Result<libc::stat> {
    let name = component_cstring(name)?;
    let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: the descriptor and NUL-terminated name remain valid, and status is writable.
    let result = unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            status.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result == 0 {
        // SAFETY: fstatat initialized status on success.
        Ok(unsafe { status.assume_init() })
    } else {
        Err(io::Error::last_os_error())
    }
}

fn temporary_name(target: &OsStr, nonce: [u8; 16]) -> Result<CString, SecureFilesystemError> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut name = target.as_bytes().to_vec();
    name.push(b'.');
    for byte in nonce {
        name.push(HEX[usize::from(byte >> 4)]);
        name.push(HEX[usize::from(byte & 0x0f)]);
    }
    name.extend_from_slice(b".tmp");
    CString::new(name).map_err(|_| SecureFilesystemError::Unsafe)
}

fn quarantine_name(name: &OsStr) -> Result<CString, SecureFilesystemError> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut nonce = [0_u8; 16];
    getrandom::fill(&mut nonce).map_err(|_| SecureFilesystemError::Unavailable)?;
    let mut quarantine = name.as_bytes().to_vec();
    quarantine.extend_from_slice(b".spaceterm-quarantine.");
    for byte in nonce {
        quarantine.push(HEX[usize::from(byte >> 4)]);
        quarantine.push(HEX[usize::from(byte & 0x0f)]);
    }
    CString::new(quarantine).map_err(|_| SecureFilesystemError::Unsafe)
}

fn component_cstring(name: &OsStr) -> io::Result<CString> {
    if name.is_empty() || name.as_bytes().contains(&b'/') {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    CString::new(name.as_bytes()).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
}

fn open_directory_at(parent: &File, name: &OsStr) -> io::Result<File> {
    let name = component_cstring(name)?;
    open_file_at_cstring(parent, &name, libc::O_RDONLY | libc::O_DIRECTORY, 0)
}

fn create_directory_at(parent: &File, name: &OsStr) -> io::Result<()> {
    let name = component_cstring(name)?;
    // SAFETY: the descriptor and NUL-terminated name remain valid for this call.
    let result = unsafe {
        libc::mkdirat(
            parent.as_raw_fd(),
            name.as_ptr(),
            PRIVATE_DIRECTORY_MODE as libc::mode_t,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn open_file_at(parent: &File, name: &OsStr, flags: i32, mode: u32) -> io::Result<File> {
    let name = component_cstring(name)?;
    open_file_at_cstring(parent, &name, flags, mode)
}

fn open_file_at_cstring(parent: &File, name: &CString, flags: i32, mode: u32) -> io::Result<File> {
    // SAFETY: the descriptor and NUL-terminated name remain valid. A successful descriptor is owned below.
    let descriptor = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            mode,
        )
    };
    if descriptor < 0 {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: openat returned a new owned descriptor.
        Ok(unsafe { File::from_raw_fd(descriptor) })
    }
}

fn set_file_mode(file: &File, mode: u32) -> io::Result<()> {
    // SAFETY: file owns a valid descriptor.
    let result = unsafe { libc::fchmod(file.as_raw_fd(), mode as libc::mode_t) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn set_entry_mode_at(parent: &File, name: &OsStr, mode: u32) -> io::Result<()> {
    let name = component_cstring(name)?;
    // SAFETY: the descriptor and NUL-terminated name remain valid for this call.
    let result = unsafe {
        libc::fchmodat(
            parent.as_raw_fd(),
            name.as_ptr(),
            mode as libc::mode_t,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn swap_at(parent: &File, source: &CString, target: &OsStr) -> io::Result<()> {
    let target = component_cstring(target)?;
    // SAFETY: the descriptor and both NUL-terminated names remain valid for this call.
    let result = unsafe {
        libc::renameatx_np(
            parent.as_raw_fd(),
            source.as_ptr(),
            parent.as_raw_fd(),
            target.as_ptr(),
            libc::RENAME_SWAP,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn rename_exclusive_at(parent: &File, source: &OsStr, target: &CString) -> io::Result<()> {
    let source = component_cstring(source)?;
    // SAFETY: the descriptor and both NUL-terminated names remain valid for this call.
    let result = unsafe {
        libc::renameatx_np(
            parent.as_raw_fd(),
            source.as_ptr(),
            parent.as_raw_fd(),
            target.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn remove_at(parent: &File, name: &OsStr, flags: i32) -> io::Result<()> {
    let name = component_cstring(name)?;
    unlink_at_cstring(parent, &name, flags)
}

fn unlink_at_cstring(parent: &File, name: &CString, flags: i32) -> io::Result<()> {
    // SAFETY: the descriptor and NUL-terminated name remain valid for this call.
    let result = unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), flags) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[derive(Default)]
struct DirectoryRollback {
    created: Vec<(File, OsString, NativeIdentity)>,
    active: bool,
}

impl DirectoryRollback {
    fn record(&mut self, parent: &File, name: &OsStr, identity: NativeIdentity) -> io::Result<()> {
        self.active = true;
        self.created
            .push((parent.try_clone()?, name.to_os_string(), identity));
        Ok(())
    }

    fn disarm(&mut self) {
        self.active = false;
    }
}

impl Drop for DirectoryRollback {
    fn drop(&mut self) {
        if self.active {
            for (parent, name, identity) in self.created.iter().rev() {
                let _ = quarantine_and_remove(parent, name, *identity, EntryKind::Directory);
            }
        }
    }
}

fn effective_user_id() -> u32 {
    // SAFETY: geteuid takes no arguments and has no preconditions.
    unsafe { libc::geteuid() }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_TEST: AtomicU64 = AtomicU64::new(0);

    fn test_root(label: &str) -> PathBuf {
        let sequence = NEXT_TEST.fetch_add(1, Ordering::Relaxed);
        fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "spaceterm-secure-fs-{}-{sequence}-{label}",
                std::process::id()
            ))
    }

    fn prepared_path(root: &Path, prepared: &PreparedPrivateFile) -> PathBuf {
        let prepared = prepared
            .0
            .downcast_ref::<NativePreparedFile>()
            .expect("native prepared file");
        root.join(OsStr::from_bytes(prepared.temporary_name.as_bytes()))
    }

    #[test]
    fn ensure_should_reject_symlinked_traversal() {
        let root = test_root("symlink");
        let outside = test_root("outside");
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE)).unwrap();
        fs::create_dir_all(&outside).unwrap();
        symlink(&outside, root.join("linked")).unwrap();

        let result = MacosSecureFilesystem.ensure_private_directory(&root.join("linked/child"));

        assert!(matches!(result, Err(SecureFilesystemError::Unsafe)));
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(outside);
    }

    #[test]
    fn read_should_reject_hard_linked_private_file() {
        let root = test_root("hard-link");
        let filesystem = MacosSecureFilesystem;
        let directory = filesystem.ensure_private_directory(&root).unwrap();
        filesystem
            .create_private_artifact(&directory, OsStr::new("config"))
            .unwrap();
        fs::hard_link(root.join("config"), root.join("second")).unwrap();

        let result = filesystem.read_private_file(&directory, OsStr::new("config"), 1024);

        assert!(matches!(result, Err(SecureFilesystemError::Unsafe)));
        let _ = fs::remove_file(root.join("second"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn ensure_should_restrict_the_owned_directory() {
        let root = test_root("permission");
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();

        MacosSecureFilesystem
            .ensure_private_directory(&root)
            .unwrap();

        assert_eq!(
            fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn commit_should_detect_identity_replacement() {
        let root = test_root("replacement");
        let filesystem = MacosSecureFilesystem;
        let directory = filesystem.ensure_private_directory(&root).unwrap();
        let first = filesystem
            .prepare_private_file(&directory, OsStr::new("config"), b"first", [1; 16])
            .unwrap();
        assert_eq!(
            filesystem.commit_private_file(first, None).unwrap(),
            SecureCommitOutcome::Committed
        );
        let snapshot = filesystem
            .read_private_file(&directory, OsStr::new("config"), 1024)
            .unwrap()
            .unwrap();
        let stale = filesystem
            .prepare_private_file(&directory, OsStr::new("config"), b"stale", [2; 16])
            .unwrap();
        let replacement = filesystem
            .prepare_private_file(&directory, OsStr::new("config"), b"replacement", [3; 16])
            .unwrap();
        assert_eq!(
            filesystem
                .commit_private_file(replacement, Some(&snapshot.identity))
                .unwrap(),
            SecureCommitOutcome::Committed
        );

        let result = filesystem
            .commit_private_file(stale, Some(&snapshot.identity))
            .unwrap();

        assert_eq!(result, SecureCommitOutcome::Conflict);
        assert_eq!(fs::read(root.join("config")).unwrap(), b"replacement");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn publication_should_exclude_a_second_writer_until_validation_finishes() {
        let root = test_root("concurrent-publication");
        let filesystem = MacosSecureFilesystem;
        let directory = filesystem.ensure_private_directory(&root).unwrap();
        let initial = filesystem
            .prepare_private_file(&directory, OsStr::new("config"), b"initial", [20; 16])
            .unwrap();
        filesystem.commit_private_file(initial, None).unwrap();
        let snapshot = filesystem
            .read_private_file(&directory, OsStr::new("config"), 1024)
            .unwrap()
            .unwrap();
        let first = filesystem
            .prepare_private_file(&directory, OsStr::new("config"), b"first", [21; 16])
            .unwrap();
        let (published, publication) = std::sync::mpsc::sync_channel(1);
        let (resume, resumed) = std::sync::mpsc::sync_channel(1);
        let first_writer = std::thread::spawn(move || {
            AFTER_PRIVATE_FILE_PUBLISH_HOOK.with(|slot| {
                *slot.borrow_mut() = Some(Box::new(move || {
                    published.send(()).unwrap();
                    resumed.recv().unwrap();
                }));
            });
            filesystem.commit_private_file(first, Some(&snapshot.identity))
        });
        publication.recv().unwrap();

        let competing_lock = File::open(&root).unwrap();
        let lock_result = competing_lock.try_lock();
        let excluded = matches!(lock_result, Err(std::fs::TryLockError::WouldBlock));
        drop(competing_lock);
        let second_root = root.clone();
        let second_writer = std::thread::spawn(move || {
            let directory = filesystem
                .open_private_directory(&second_root)
                .unwrap()
                .unwrap();
            let snapshot = filesystem
                .read_private_file(&directory, OsStr::new("config"), 1024)
                .unwrap()
                .unwrap();
            let mut bytes = snapshot.bytes;
            bytes.extend_from_slice(b"+second");
            let second = filesystem
                .prepare_private_file(&directory, OsStr::new("config"), &bytes, [22; 16])
                .unwrap();
            filesystem.commit_private_file(second, Some(&snapshot.identity))
        });
        resume.send(()).unwrap();
        let first_result = first_writer.join().unwrap();
        let second_result = second_writer.join().unwrap();

        assert!(
            excluded,
            "publication must retain exclusion through validation"
        );
        assert_eq!(first_result.unwrap(), SecureCommitOutcome::Committed);
        assert_eq!(second_result.unwrap(), SecureCommitOutcome::Committed);
        assert_eq!(fs::read(root.join("config")).unwrap(), b"first+second");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rollback_should_preserve_a_successor_that_ignores_the_transaction_lock() {
        let root = test_root("rollback-successor");
        let filesystem = MacosSecureFilesystem;
        let directory = filesystem.ensure_private_directory(&root).unwrap();
        let initial = filesystem
            .prepare_private_file(&directory, OsStr::new("config"), b"initial", [23; 16])
            .unwrap();
        filesystem.commit_private_file(initial, None).unwrap();
        let snapshot = filesystem
            .read_private_file(&directory, OsStr::new("config"), 1024)
            .unwrap()
            .unwrap();
        let replacement = filesystem
            .prepare_private_file(&directory, OsStr::new("config"), b"replacement", [24; 16])
            .unwrap();
        let predecessor = prepared_path(&root, &replacement);
        let successor = root.join("successor");
        fs::write(&successor, b"successor").unwrap();
        fs::set_permissions(&successor, fs::Permissions::from_mode(PRIVATE_FILE_MODE)).unwrap();
        let target = root.join("config");
        AFTER_PRIVATE_FILE_PUBLISH_HOOK.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || {
                fs::rename(successor, target).unwrap();
            }));
        });

        let result = filesystem.commit_private_file(replacement, Some(&snapshot.identity));

        assert!(result.is_err());
        assert_eq!(fs::read(root.join("config")).unwrap(), b"successor");
        assert_eq!(fs::read(predecessor).unwrap(), b"initial");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn first_publication_rollback_should_preserve_an_unowned_successor() {
        let root = test_root("first-publication-successor");
        let filesystem = MacosSecureFilesystem;
        let directory = filesystem.ensure_private_directory(&root).unwrap();
        let prepared = filesystem
            .prepare_private_file(&directory, OsStr::new("config"), b"first", [25; 16])
            .unwrap();
        let successor = root.join("successor");
        fs::write(&successor, b"successor").unwrap();
        fs::set_permissions(&successor, fs::Permissions::from_mode(PRIVATE_FILE_MODE)).unwrap();
        let target = root.join("config");
        AFTER_PRIVATE_FILE_PUBLISH_HOOK.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || {
                fs::rename(successor, target).unwrap();
            }));
        });

        let result = filesystem.commit_private_file(prepared, None);

        assert!(result.is_err());
        assert_eq!(fs::read(root.join("config")).unwrap(), b"successor");
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn create_should_not_publish_a_hard_linked_prepared_inode() {
        let root = test_root("create-hard-linked-prepared");
        let filesystem = MacosSecureFilesystem;
        let directory = filesystem.ensure_private_directory(&root).unwrap();
        let prepared = filesystem
            .prepare_private_file(&directory, OsStr::new("config"), b"new", [4; 16])
            .unwrap();
        let prepared_path = prepared_path(&root, &prepared);
        let hook_prepared_path = prepared_path.clone();
        let attacker_link = root.join("attacker-link");
        let hook_attacker_link = attacker_link.clone();
        install_before_private_file_publish_hook(move || {
            fs::hard_link(hook_prepared_path, hook_attacker_link).unwrap();
        });

        let result = filesystem.commit_private_file(prepared, None);

        assert!(matches!(result, Err(SecureFilesystemError::Unsafe)));
        assert!(!root.join("config").exists());
        assert!(!prepared_path.exists());
        assert_eq!(fs::read(&attacker_link).unwrap(), b"new");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn update_should_restore_target_when_prepared_inode_is_hard_linked() {
        let root = test_root("update-hard-linked-prepared");
        let filesystem = MacosSecureFilesystem;
        let directory = filesystem.ensure_private_directory(&root).unwrap();
        let original = filesystem
            .prepare_private_file(&directory, OsStr::new("config"), b"original", [5; 16])
            .unwrap();
        assert_eq!(
            filesystem.commit_private_file(original, None).unwrap(),
            SecureCommitOutcome::Committed
        );
        let snapshot = filesystem
            .read_private_file(&directory, OsStr::new("config"), 1024)
            .unwrap()
            .unwrap();
        let prepared = filesystem
            .prepare_private_file(&directory, OsStr::new("config"), b"replacement", [6; 16])
            .unwrap();
        let prepared_path = prepared_path(&root, &prepared);
        let hook_prepared_path = prepared_path.clone();
        let attacker_link = root.join("attacker-link");
        let hook_attacker_link = attacker_link.clone();
        install_before_private_file_publish_hook(move || {
            fs::hard_link(hook_prepared_path, hook_attacker_link).unwrap();
        });

        let result = filesystem.commit_private_file(prepared, Some(&snapshot.identity));

        assert!(matches!(result, Err(SecureFilesystemError::Unsafe)));
        assert_eq!(fs::read(root.join("config")).unwrap(), b"original");
        assert!(!prepared_path.exists());
        assert_eq!(fs::read(&attacker_link).unwrap(), b"replacement");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn socket_cleanup_should_remove_only_the_registered_identity() {
        let root = test_root("socket");
        let filesystem = MacosSecureFilesystem;
        let directory = filesystem.ensure_private_directory(&root).unwrap();
        let path = root.join("endpoint");
        let listener = UnixListener::bind(&path).unwrap();
        let identity = filesystem
            .register_socket(&directory, OsStr::new("endpoint"))
            .unwrap();
        filesystem
            .verify_socket(&directory, OsStr::new("endpoint"), &identity)
            .unwrap();

        filesystem
            .remove_socket(&directory, OsStr::new("endpoint"), &identity)
            .unwrap();

        assert!(!path.exists());
        drop(listener);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn prepared_cleanup_should_restore_a_regular_file_replacement() {
        let root = test_root("prepared-regular-replacement");
        let filesystem = MacosSecureFilesystem;
        let directory = filesystem.ensure_private_directory(&root).unwrap();
        let prepared = filesystem
            .prepare_private_file(&directory, OsStr::new("config"), b"owned", [7; 16])
            .unwrap();
        let path = prepared_path(&root, &prepared);
        let owned = root.join("owned-backup");
        fs::rename(&path, &owned).unwrap();
        fs::write(&path, b"replacement").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(PRIVATE_FILE_MODE)).unwrap();

        drop(prepared);

        assert_eq!(fs::read(&path).unwrap(), b"replacement");
        assert_eq!(fs::read(&owned).unwrap(), b"owned");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn commit_should_reject_and_restore_a_replaced_prepared_path() {
        let root = test_root("commit-prepared-replacement");
        let filesystem = MacosSecureFilesystem;
        let directory = filesystem.ensure_private_directory(&root).unwrap();
        let prepared = filesystem
            .prepare_private_file(&directory, OsStr::new("config"), b"owned", [10; 16])
            .unwrap();
        let path = prepared_path(&root, &prepared);
        let owned = root.join("owned-backup");
        fs::rename(&path, &owned).unwrap();
        fs::write(&path, b"replacement").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(PRIVATE_FILE_MODE)).unwrap();

        let result = filesystem.commit_private_file(prepared, None);

        assert!(matches!(result, Err(SecureFilesystemError::Unsafe)));
        assert_eq!(fs::read(&path).unwrap(), b"replacement");
        assert!(!root.join("config").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn prepared_cleanup_should_restore_a_symlink_replacement() {
        let root = test_root("prepared-symlink-replacement");
        let filesystem = MacosSecureFilesystem;
        let directory = filesystem.ensure_private_directory(&root).unwrap();
        let prepared = filesystem
            .prepare_private_file(&directory, OsStr::new("config"), b"owned", [8; 16])
            .unwrap();
        let path = prepared_path(&root, &prepared);
        let owned = root.join("owned-backup");
        let outside = root.join("outside");
        fs::rename(&path, &owned).unwrap();
        fs::write(&outside, b"outside").unwrap();
        symlink(&outside, &path).unwrap();

        drop(prepared);

        assert_eq!(fs::read_link(&path).unwrap(), outside);
        assert_eq!(fs::read(&owned).unwrap(), b"owned");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn prepared_cleanup_should_restore_a_hard_link_replacement() {
        let root = test_root("prepared-hard-link-replacement");
        let filesystem = MacosSecureFilesystem;
        let directory = filesystem.ensure_private_directory(&root).unwrap();
        let prepared = filesystem
            .prepare_private_file(&directory, OsStr::new("config"), b"owned", [9; 16])
            .unwrap();
        let path = prepared_path(&root, &prepared);
        let owned = root.join("owned-backup");
        let outside = root.join("outside");
        fs::rename(&path, &owned).unwrap();
        fs::write(&outside, b"outside").unwrap();
        fs::set_permissions(&outside, fs::Permissions::from_mode(PRIVATE_FILE_MODE)).unwrap();
        fs::hard_link(&outside, &path).unwrap();

        drop(prepared);

        assert_eq!(fs::read(&path).unwrap(), b"outside");
        assert_eq!(fs::metadata(&outside).unwrap().nlink(), 2);
        assert_eq!(fs::read(&owned).unwrap(), b"owned");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn socket_cleanup_should_restore_a_replacement_socket() {
        let root = test_root("r");
        let filesystem = MacosSecureFilesystem;
        let directory = filesystem.ensure_private_directory(&root).unwrap();
        let path = root.join("endpoint");
        let original = UnixListener::bind(&path).unwrap();
        let identity = filesystem
            .register_socket(&directory, OsStr::new("endpoint"))
            .unwrap();
        fs::remove_file(&path).unwrap();
        let replacement = UnixListener::bind(&path).unwrap();

        let result = filesystem.remove_socket(&directory, OsStr::new("endpoint"), &identity);

        assert!(matches!(result, Err(SecureFilesystemError::Unsafe)));
        assert!(path.exists());
        drop(replacement);
        drop(original);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn directory_cleanup_should_restore_a_replacement_directory() {
        let root = test_root("directory-replacement");
        let filesystem = MacosSecureFilesystem;
        let parent = filesystem.ensure_private_directory(&root).unwrap();
        let child = filesystem
            .create_private_child(&parent, OsStr::new("owner"))
            .unwrap();
        fs::rename(root.join("owner"), root.join("owned-backup")).unwrap();
        fs::create_dir(root.join("owner")).unwrap();
        fs::set_permissions(
            root.join("owner"),
            fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE),
        )
        .unwrap();

        let result = filesystem.remove_private_child(&parent, OsStr::new("owner"), &child);

        assert!(matches!(result, Err(SecureFilesystemError::Unsafe)));
        assert!(root.join("owner").is_dir());
        assert!(root.join("owned-backup").is_dir());
        let _ = fs::remove_dir_all(root);
    }
}
