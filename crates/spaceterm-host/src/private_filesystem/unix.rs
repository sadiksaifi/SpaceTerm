use sha2::{Digest as _, Sha256};

// Share the desktop engine without maintaining a second file transaction implementation.
include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../src/platform/unix_secure_filesystem.rs"
));

pub(super) fn verify_directory_acl(handle: &SecureDirectory) -> Result<(), super::Failure> {
    let directory = directory(handle)?;
    let _admission = open_mutation(&directory.file, OsStr::new(MUTATION_NAME), false)?;
    verify_directory_entry(&directory)?;
    verify_acl(&directory.file).map_err(Into::into)
}

pub(super) fn verify_established(handle: &SecureDirectory) -> Result<(), super::Failure> {
    let directory = directory(handle)?;
    let _admission = open_mutation(&directory.file, OsStr::new(MUTATION_NAME), false)?
        .ok_or(super::Failure::Indeterminate)?;
    verify_directory_entry(&directory)?;
    verify_acl(&directory.file).map_err(Into::into)
}

// Account records can contain bearer bodies. The desktop read operation uses an
// ordinary Vec and may return after a failed post-read identity check. Keep the
// same native descriptor, lock and version mechanics but own this read buffer in
// Zeroizing from allocation, including every error path.
pub(super) fn read_zeroizing(
    handle: &SecureDirectory,
    name: &OsStr,
    maximum: usize,
) -> Result<Option<super::Snapshot>, super::Failure> {
    let directory = directory(handle)?;
    let admission = open_mutation(&directory.file, OsStr::new(MUTATION_NAME), false)?;
    let _transaction = lock_private_directory(&directory)?;
    verify_directory_entry(&directory)?;
    verify_acl(&directory.file)?;
    if admission.is_none() && file_version_at(&directory.file, name)?.is_some() {
        return Err(super::Failure::Indeterminate);
    }
    read_unfenced(&directory, name, maximum)
}

fn read_unfenced(
    directory: &NativeDirectory,
    name: &OsStr,
    maximum: usize,
) -> Result<Option<super::Snapshot>, super::Failure> {
    let file = match open_file_at(&directory.file, name, libc::O_RDONLY | libc::O_NONBLOCK, 0) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(classify(error).into()),
    };
    let version = private_file_version(&file.metadata().map_err(classify)?)?;
    verify_acl(&file)?;
    let mut bytes = zeroize::Zeroizing::new(Vec::new());
    (&file)
        .take(maximum.saturating_add(1) as u64)
        .read_to_end(&mut bytes)
        .map_err(classify)?;
    let after = private_file_version(&file.metadata().map_err(classify)?)?;
    if version != after {
        return Err(super::Failure::Unsafe);
    }
    verify_directory_entry(directory)?;
    verify_acl(&file)?;
    if file_version_at(&directory.file, name)? != Some(version) {
        return Err(super::Failure::Unsafe);
    }
    let identity = NativeFileIdentity {
        version,
        fingerprint: fingerprint(&bytes),
        fingerprint_bytes: bytes.len() as u64,
    };
    Ok(Some(super::Snapshot {
        bytes,
        identity: SecureEntryIdentity(Arc::new(identity)),
    }))
}

// Keep the descriptor transaction held through the deletion's durability barrier.
// The shared removal primitive intentionally does not promise directory synchronization.
pub(super) fn remove_and_sync(
    handle: &SecureDirectory,
    name: &OsStr,
    expected_handle: &SecureEntryIdentity,
) -> Result<(), super::Failure> {
    let directory = directory(handle)?;
    let expected = *file_snapshot_identity(expected_handle)?;
    let _transaction = lock_private_directory(&directory)?;
    verify_directory_entry(&directory)?;
    verify_acl(&directory.file)?;
    if file_version_at(&directory.file, name)?.is_none() {
        return Err(super::Failure::Missing);
    }
    if !file_matches_snapshot_at(&directory.file, name, expected)? {
        return Err(super::Failure::Conflict);
    }
    quarantine_and_remove_retained_file(&directory.file, name, expected.version.entry)
        .map_err(|_| super::Failure::Indeterminate)?;
    #[cfg(feature = "test-support")]
    super::test_support::removal_sync().map_err(|_| super::Failure::Indeterminate)?;
    directory
        .file
        .sync_all()
        .map_err(|_| super::Failure::Indeterminate)?;
    verify_directory_entry(&directory).map_err(|_| super::Failure::Indeterminate)
}

pub(super) fn verify_prepared_acl(
    prepared: &PreparedPrivateFile,
) -> Result<(), SecureFilesystemError> {
    let prepared = prepared
        .0
        .downcast_ref::<NativePreparedFile>()
        .ok_or(SecureFilesystemError::Unsafe)?;
    verify_acl(&prepared.file)
}

// Initial creation is a different policy from desktop permission repair. Reuse
// the engine's no-follow descriptor operations, never chmod an existing entry,
// and synchronize every new directory and its parent before returning authority.
pub(super) fn create_without_repair(path: &Path) -> Result<SecureDirectory, super::Failure> {
    if !path.is_absolute() {
        return Err(super::Failure::Unsafe);
    }
    let mut parent = File::open("/").map_err(|_| super::Failure::Unavailable)?;
    let mut components = path.components().peekable();
    while let Some(component) = components.next() {
        let Component::Normal(name) = component else {
            if matches!(component, Component::RootDir) {
                continue;
            }
            return Err(super::Failure::Unsafe);
        };
        let final_entry = components.peek().is_none();
        let (file, created, mut initialization) = match open_directory_at(&parent, name) {
            Ok(file) => (file, false, None),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let facts = parent.metadata().map_err(classify)?;
                if facts.uid() != effective_user_id() {
                    return Err(super::Failure::Unsafe);
                }
                let phase = begin_mutation(
                    &parent,
                    &initialization_name(name),
                    mutation_record(3, name, [0; 32])?,
                )?;
                #[cfg(feature = "test-support")]
                super::test_support::creation_before_effect()?;
                match create_directory_at(&parent, name) {
                    Ok(()) => {
                        let file = open_directory_at(&parent, name)
                            .map_err(|_| super::Failure::Indeterminate)?;
                        (file, true, Some(phase))
                    }
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (
                        open_directory_at(&parent, name)
                            .map_err(|error| super::Failure::from(classify(error)))?,
                        false,
                        Some(phase),
                    ),
                    Err(error) => return Err(classify(error).into()),
                }
            }
            Err(error) => return Err(classify(error).into()),
        };
        if created {
            set_file_mode(&file, PRIVATE_DIRECTORY_MODE)
                .map_err(|_| super::Failure::Indeterminate)?;
            private_directory_identity(&file)
                .map_err(|error| super::Failure::from(classify(error)))?;
            verify_acl(&file)?;
            file.sync_all().map_err(|_| super::Failure::Indeterminate)?;
            #[cfg(feature = "test-support")]
            super::test_support::creation_parent_sync()
                .map_err(|_| super::Failure::Indeterminate)?;
            parent
                .sync_all()
                .map_err(|_| super::Failure::Indeterminate)?;
        }
        if let Some(phase) = &mut initialization {
            // A concurrent winner can already have created the exact private directory.
            private_directory_identity(&file)
                .map_err(|error| super::Failure::from(classify(error)))?;
            verify_acl(&file)?;
            phase.finish()?;
        }
        drop(initialization);
        let _initialization = open_mutation(&parent, &initialization_name(name), false)?;
        if final_entry {
            let identity = private_directory_identity(&file)
                .map_err(|error| super::Failure::from(classify(error)))?;
            verify_acl(&file)?;
            return Ok(wrap_directory(NativeDirectory {
                parent,
                file,
                name: name.to_owned(),
                identity,
            }));
        }
        parent = file;
    }
    Err(super::Failure::Unsafe)
}

#[cfg(target_os = "linux")]
fn verify_acl(_file: &File) -> Result<(), SecureFilesystemError> {
    // POSIX ACL access is constrained by the mode mask already checked by the engine.
    Ok(())
}

#[cfg(target_os = "macos")]
fn verify_acl(file: &File) -> Result<(), SecureFilesystemError> {
    use std::ffi::c_void;
    unsafe extern "C" {
        fn acl_get_fd_np(fd: libc::c_int, kind: libc::c_int) -> *mut c_void;
        fn acl_free(acl: *mut c_void) -> libc::c_int;
    }
    // macOS extended ACL entries can grant access beyond mode 0700/0600. Reject
    // every extended entry, including inherited entries, rather than infer access.
    // SAFETY: file is a live retained descriptor and ACL_TYPE_EXTENDED is 0x100.
    let acl = unsafe { acl_get_fd_np(file.as_raw_fd(), 0x100) };
    if acl.is_null() {
        // The descriptor is live and independently identity-checked. macOS
        // reports ENOENT for an absent extended ACL on that existing object.
        return if io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT) {
            Ok(())
        } else {
            Err(SecureFilesystemError::Unavailable)
        };
    }
    // SAFETY: acl_get_fd_np allocated this ACL; no entry is used after release.
    let freed = unsafe { acl_free(acl) };
    if freed != 0 {
        return Err(SecureFilesystemError::Unavailable);
    }
    // A separate extended ACL is never required for these account-private files.
    // Conservatively reject an ACL object rather than interpreting its entries.
    Err(SecureFilesystemError::Unsafe)
}

// One retained, nonsecret write-ahead phase per account-private directory. The
// phase is durable before a public entry can change. A failed effect sync leaves
// Pending in place across handles/processes. Opening never repairs or clears it.
// Completion is a separately prepared, synchronized and nonce-bound checkpoint.
const MUTATION_NAME: &str = ".account-mutation";
const MUTATION_BYTES: usize = 88;

pub(super) struct MutationGuard {
    file: File,
    parent: File,
    name: OsString,
    identity: NativeIdentity,
}

impl MutationGuard {
    fn check(&self) -> Result<(), super::Failure> {
        if private_file_identity(&self.file.metadata().map_err(classify)?)? != self.identity
            || file_identity_at(&self.parent, &self.name)? != Some(self.identity)
        {
            return Err(super::Failure::Unsafe);
        }
        verify_acl(&self.file)?;
        Ok(())
    }

    fn finish(&mut self) -> Result<(), super::Failure> {
        self.check()?;
        let mut record = read_phase(&self.file)?;
        if record[5] != 1 {
            return Err(super::Failure::Indeterminate);
        }
        if record[4] == 2 {
            remove_proposal(&self.parent, &record)?;
        }
        // Pending remains unchanged. Prepare and synchronize a separate checkpoint
        // before making it visible. Completion I/O failure cannot expose Clean in
        // the original fence, and needs no best-effort rollback of that fence.
        record[5] = 0;
        publish_checkpoint(&self.parent, &completion_name(&self.name), &record)?;
        self.check()
    }
}

fn mutation_record(
    operation: u8,
    target: &OsStr,
    digest: [u8; 32],
) -> Result<[u8; MUTATION_BYTES], super::Failure> {
    let mut record = [0; MUTATION_BYTES];
    record[..8].copy_from_slice(&[b'S', b'T', b'M', b'F', 1, 1, operation, 0]);
    getrandom::fill(&mut record[8..24]).map_err(|_| super::Failure::Unavailable)?;
    if record[8..24] == [0; 16] {
        return Err(super::Failure::Unavailable);
    }
    record[24..56].copy_from_slice(&Sha256::digest(target.as_bytes()));
    record[56..88].copy_from_slice(&digest);
    Ok(record)
}

fn open_mutation(
    parent: &File,
    name: &OsStr,
    writable: bool,
) -> Result<Option<MutationGuard>, super::Failure> {
    let flags = if writable {
        libc::O_RDWR
    } else {
        libc::O_RDONLY
    };
    let file = match open_file_at(parent, name, flags | libc::O_NONBLOCK, 0) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(classify(error).into()),
    };
    let identity = private_file_identity(&file.metadata().map_err(classify)?)?;
    file.lock().map_err(classify)?;
    let guard = MutationGuard {
        file,
        parent: parent.try_clone().map_err(classify)?,
        name: name.to_owned(),
        identity,
    };
    guard.check()?;
    let mut record = read_phase(&guard.file)?;
    // A Clean byte in the original fence is never sufficient. The checkpoint
    // must bind this exact nonce, operation, target and admitted content digest.
    // It can reach its public name only after successful file synchronization.
    let completion = open_file_at(
        parent,
        &completion_name(name),
        libc::O_RDONLY | libc::O_NONBLOCK,
        0,
    )
    .map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => super::Failure::Indeterminate,
        _ => classify(error).into(),
    })?;
    let version = private_file_version(&completion.metadata().map_err(classify)?)?;
    verify_acl(&completion)?;
    let checkpoint = read_phase(&completion)?;
    record[5] = 0;
    if checkpoint != record
        || file_version_at(parent, &completion_name(name))? != Some(version)
        || private_file_version(&completion.metadata().map_err(classify)?)? != version
    {
        return Err(super::Failure::Indeterminate);
    }
    guard.check()?;
    Ok(Some(guard))
}

fn read_phase(file: &File) -> Result<[u8; MUTATION_BYTES], super::Failure> {
    let mut record = [0; MUTATION_BYTES];
    if file.metadata().map_err(classify)?.len() != MUTATION_BYTES as u64
        || file.read_exact_at(&mut record, 0).is_err()
        || record[..4] != *b"STMF"
        || !(1..=2).contains(&record[4])
        || record[5] > 1
        || !(1..=3).contains(&record[6])
        || (record[4] == 1 && record[7] != 0)
        || (record[4] == 2 && (record[6] != 1 || record[7] > 2))
        || record[8..24] == [0; 16]
    {
        return Err(super::Failure::Indeterminate);
    }
    Ok(record)
}

fn completion_name(name: &OsStr) -> OsString {
    let mut name = name.to_owned();
    name.push(".complete");
    name
}

fn publish_checkpoint(
    parent: &File,
    name: &OsStr,
    record: &[u8; MUTATION_BYTES],
) -> Result<(), super::Failure> {
    #[cfg(feature = "test-support")]
    let _completion_io = super::test_support::CompletionIoScope;
    let expected = file_version_at(parent, name)?;
    let mut nonce = [0; 16];
    getrandom::fill(&mut nonce).map_err(|_| super::Failure::Unavailable)?;
    let temporary = temporary_name(name, nonce)?;
    let temporary_name = OsStr::from_bytes(temporary.as_bytes());
    let file = open_file_at(
        parent,
        temporary_name,
        libc::O_RDWR | libc::O_CREAT | libc::O_EXCL,
        PRIVATE_FILE_MODE,
    )
    .map_err(classify)?;
    let identity = metadata_identity(&file.metadata().map_err(classify)?);
    let result = (|| {
        set_file_mode(&file, PRIVATE_FILE_MODE).map_err(classify)?;
        private_file_identity(&file.metadata().map_err(classify)?)?;
        verify_acl(&file)?;
        #[cfg(feature = "test-support")]
        super::test_support::mutation_completion_write()
            .map_err(|_| super::Failure::Indeterminate)?;
        file.write_all_at(record, 0)
            .map_err(|_| super::Failure::Indeterminate)?;
        #[cfg(feature = "test-support")]
        super::test_support::mutation_completion_sync()
            .map_err(|_| super::Failure::Indeterminate)?;
        file.sync_all().map_err(|_| super::Failure::Indeterminate)?;
        if file_identity_at(parent, temporary_name)? != Some(identity)
            || file_version_at(parent, name)? != expected
        {
            return Err(super::Failure::Unsafe);
        }
        let published = private_file_version(&file.metadata().map_err(classify)?)?;
        if let Some(expected) = expected {
            swap_at(parent, &temporary, name).map_err(|_| super::Failure::Indeterminate)?;
            if file_version_at(parent, temporary_name)? != Some(expected)
                || file_version_at(parent, name)? != Some(published)
            {
                return Err(super::Failure::Indeterminate);
            }
            quarantine_and_remove_retained_file(parent, temporary_name, expected.entry)
                .map_err(|_| super::Failure::Indeterminate)?;
        } else {
            let target = component_cstring(name).map_err(classify)?;
            rename_exclusive_at(parent, temporary_name, &target)
                .map_err(|_| super::Failure::Indeterminate)?;
        }
        parent
            .sync_all()
            .map_err(|_| super::Failure::Indeterminate)?;
        if file_version_at(parent, name)? != Some(published) {
            return Err(super::Failure::Indeterminate);
        }
        Ok(())
    })();
    // A failed temporary checkpoint is not the original Pending fence. Cleanup
    // may fail without reopening admission; no readable public Clean was written.
    let _ = quarantine_and_remove_retained_file(parent, temporary_name, identity);
    result
}

fn begin_mutation(
    parent: &File,
    name: &OsStr,
    record: [u8; MUTATION_BYTES],
) -> Result<MutationGuard, super::Failure> {
    if let Some(guard) = open_mutation(parent, name, true)? {
        guard
            .file
            .write_all_at(&record, 0)
            .and_then(|()| guard.file.sync_all())
            .map_err(|_| super::Failure::Indeterminate)?;
        guard.check()?;
        return Ok(guard);
    }
    // Publish a fully prepared Pending record, never a visible empty lock file.
    // This also handles nonprivate OS-home ancestors during first initialization.
    let mut nonce = [0; 16];
    getrandom::fill(&mut nonce).map_err(|_| super::Failure::Unavailable)?;
    let temporary = temporary_name(name, nonce)?;
    let temporary_name = OsStr::from_bytes(temporary.as_bytes());
    let file = open_file_at(
        parent,
        temporary_name,
        libc::O_RDWR | libc::O_CREAT | libc::O_EXCL,
        PRIVATE_FILE_MODE,
    )
    .map_err(classify)?;
    let temporary_identity = metadata_identity(&file.metadata().map_err(classify)?);
    let _retained_temporary = file.try_clone().map_err(classify)?;
    let prepare = (|| {
        set_file_mode(&file, PRIVATE_FILE_MODE).map_err(classify)?;
        let identity = private_file_identity(&file.metadata().map_err(classify)?)?;
        verify_acl(&file)?;
        file.lock().map_err(classify)?;
        file.write_all_at(&record, 0)
            .and_then(|()| file.sync_all())
            .map_err(classify)?;
        let target = component_cstring(name).map_err(classify)?;
        match rename_exclusive_at(parent, temporary_name, &target) {
            Ok(()) => {
                let guard = MutationGuard {
                    file,
                    parent: parent.try_clone().map_err(classify)?,
                    name: name.to_owned(),
                    identity,
                };
                // No entry mutation is permitted until the fence itself is durable.
                parent
                    .sync_all()
                    .map_err(|_| super::Failure::Indeterminate)?;
                guard.check()?;
                Ok(Some(guard))
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(None),
            Err(error) => Err(super::Failure::from(classify(error))),
        }
    })();
    // On losing the exclusive publication, discard only this private temporary.
    // On winning, its old name no longer exists.
    let _ = quarantine_and_remove_retained_file(parent, temporary_name, temporary_identity);
    match prepare? {
        Some(guard) => Ok(guard),
        None => {
            let guard = open_mutation(parent, name, true)?.ok_or(super::Failure::Indeterminate)?;
            guard
                .file
                .write_all_at(&record, 0)
                .and_then(|()| guard.file.sync_all())
                .map_err(|_| super::Failure::Indeterminate)?;
            guard.check()?;
            Ok(guard)
        }
    }
}

pub(super) fn begin_publication(
    handle: &SecureDirectory,
    name: &OsStr,
    bytes: &[u8],
    expected: Option<&SecureEntryIdentity>,
) -> Result<MutationGuard, super::Failure> {
    if bytes.len() > 256 * 1024 || name.as_bytes().len() > 255 {
        return Err(super::Failure::LimitExceeded);
    }
    let directory = directory(handle)?;
    verify_directory_entry(&directory)?;
    let mut phase = mutation_record(1, name, fingerprint(bytes))?;
    phase[4] = 2;
    let guard = begin_mutation(&directory.file, OsStr::new(MUTATION_NAME), phase)?;
    let mut payload = zeroize::Zeroizing::new(Vec::new());
    payload.extend_from_slice(b"STPR\x01");
    payload.extend_from_slice(&phase[8..24]);
    payload.extend_from_slice(&(name.as_bytes().len() as u16).to_be_bytes());
    payload.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    payload.push(u8::from(expected.is_some()));
    payload.extend_from_slice(
        &expected
            .map(file_snapshot_identity)
            .transpose()?
            .map(snapshot_digest)
            .unwrap_or([0; 32]),
    );
    payload.extend_from_slice(name.as_bytes());
    payload.extend_from_slice(bytes);
    let old = read_unfenced(&directory, OsStr::new(PROPOSAL_NAME), PROPOSAL_LIMIT)?;
    let mut nonce = [0; 16];
    getrandom::fill(&mut nonce).map_err(|_| super::Failure::Unavailable)?;
    let prepared = UnixSecureFilesystem.prepare_private_file(
        handle,
        OsStr::new(PROPOSAL_NAME),
        &payload,
        nonce,
    )?;
    verify_prepared_acl(&prepared)?;
    let committed = UnixSecureFilesystem
        .commit_private_file(prepared, old.as_ref().map(|snapshot| &snapshot.identity))?;
    if committed.outcome != SecureCommitOutcome::Committed {
        return Err(super::Failure::Indeterminate);
    }
    // Flag 0 proves effects have not begun. Flag 1 is durable only after the
    // complete original payload and exact pre-effect version are synchronized.
    guard
        .file
        .write_all_at(&[1], 7)
        .and_then(|()| guard.file.sync_all())
        .map_err(|_| super::Failure::Indeterminate)?;
    guard.check()?;
    Ok(guard)
}

const PROPOSAL_NAME: &str = ".account-proposal";
const PROPOSAL_HEADER: usize = 60;
const PROPOSAL_LIMIT: usize = 256 * 1024 + PROPOSAL_HEADER + 255;

fn snapshot_digest(snapshot: &NativeFileIdentity) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(snapshot.fingerprint);
    digest.update(snapshot.fingerprint_bytes.to_be_bytes());
    digest.update(snapshot.version.entry.device.to_be_bytes());
    digest.update(snapshot.version.entry.inode.to_be_bytes());
    digest.update(snapshot.version.size.to_be_bytes());
    digest.update(snapshot.version.modified_seconds.to_be_bytes());
    digest.update(snapshot.version.modified_nanoseconds.to_be_bytes());
    digest.finalize().into()
}
fn remove_proposal(parent: &File, phase: &[u8; MUTATION_BYTES]) -> Result<(), super::Failure> {
    let file = match open_file_at(
        parent,
        OsStr::new(PROPOSAL_NAME),
        libc::O_RDONLY | libc::O_NONBLOCK,
        0,
    ) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(classify(error).into()),
    };
    let version = private_file_version(&file.metadata().map_err(classify)?)?;
    verify_acl(&file)?;
    if version.size > PROPOSAL_LIMIT as u64 || version.size < PROPOSAL_HEADER as u64 {
        return Err(super::Failure::Unsafe);
    }
    let mut header = [0; PROPOSAL_HEADER];
    file.read_exact_at(&mut header, 0).map_err(classify)?;
    if header[..5] != *b"STPR\x01" || header[5..21] != phase[8..24] {
        return Err(super::Failure::Indeterminate);
    }
    if file_version_at(parent, OsStr::new(PROPOSAL_NAME))? != Some(version) {
        return Err(super::Failure::Unsafe);
    }
    quarantine_and_remove_retained_file(parent, OsStr::new(PROPOSAL_NAME), version.entry)
        .map_err(|_| super::Failure::Indeterminate)?;
    parent.sync_all().map_err(|_| super::Failure::Indeterminate)
}
pub(super) fn abort_publication(guard: &mut MutationGuard) -> Result<(), super::Failure> {
    guard.check()?;
    guard
        .file
        .write_all_at(&[2], 7)
        .and_then(|()| guard.file.sync_all())
        .map_err(|_| super::Failure::Indeterminate)?;
    guard.finish()
}

pub(super) fn begin_removal(
    handle: &SecureDirectory,
    name: &OsStr,
    identity: &SecureEntryIdentity,
) -> Result<MutationGuard, super::Failure> {
    let directory = directory(handle)?;
    verify_directory_entry(&directory)?;
    let expected = file_snapshot_identity(identity)?;
    let digest = removal_digest(expected);

    begin_mutation(
        &directory.file,
        OsStr::new(MUTATION_NAME),
        mutation_record(2, name, digest)?,
    )
}

pub(super) fn finish_mutation(guard: &mut MutationGuard) -> Result<(), super::Failure> {
    guard.finish()
}

fn initialization_name(name: &OsStr) -> OsString {
    use std::fmt::Write as _;
    let mut result = String::from(".spaceterm-init-");
    for byte in Sha256::digest(name.as_bytes()) {
        let _ = write!(result, "{byte:02x}");
    }
    result.into()
}

// Each initialization fence belongs to the same retained parent descriptor used
// to open its child. Do not validate one pathname traversal and then admit another.
struct InitializationStep {
    parent: File,
    file: File,
    name: OsString,
    identity: NativeIdentity,
}

pub(super) struct InitializationAuthority(Vec<InitializationStep>);

impl InitializationAuthority {
    pub(super) fn verify(&self) -> Result<(), super::Failure> {
        for step in &self.0 {
            let _fence = open_mutation(&step.parent, &initialization_name(&step.name), false)?;
            let entry = open_directory_at(&step.parent, &step.name).map_err(classify)?;
            if metadata_identity(&entry.metadata().map_err(classify)?) != step.identity
                || metadata_identity(&step.file.metadata().map_err(classify)?) != step.identity
            {
                return Err(super::Failure::Unsafe);
            }
        }
        Ok(())
    }
}

pub(super) fn admit_existing(
    path: &Path,
) -> Result<Option<(SecureDirectory, InitializationAuthority)>, super::Failure> {
    if !path.is_absolute() {
        return Err(super::Failure::Unsafe);
    }
    let mut parent = File::open("/").map_err(classify)?;
    let mut authority = InitializationAuthority(Vec::new());
    let mut components = path.components().peekable();
    while let Some(component) = components.next() {
        let Component::Normal(name) = component else {
            if matches!(component, Component::RootDir) {
                continue;
            }
            return Err(super::Failure::Unsafe);
        };
        let file = match open_directory_at(&parent, name) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                // An absent child under Pending is unavailable initialization,
                // not permission to start an independent first installation.
                let _fence = open_mutation(&parent, &initialization_name(name), false)?;
                authority.verify()?;
                return Ok(None);
            }
            Err(error) => return Err(classify(error).into()),
        };
        let identity = metadata_identity(&file.metadata().map_err(classify)?);
        authority.0.push(InitializationStep {
            parent: parent.try_clone().map_err(classify)?,
            file: file.try_clone().map_err(classify)?,
            name: name.to_owned(),
            identity,
        });
        // The fence is inspected AFTER this exact child was opened. A concurrent
        // mkdir must first publish Pending in the retained parent, so it cannot
        // insert a new child between an absent-fence check and descriptor admission.
        authority.verify()?;
        if components.peek().is_none() {
            private_directory_identity(&file).map_err(classify)?;
            verify_acl(&file)?;
            let directory = wrap_directory(NativeDirectory {
                parent,
                file,
                name: name.to_owned(),
                identity,
            });
            return Ok(Some((directory, authority)));
        }
        parent = file;
    }
    Err(super::Failure::Unsafe)
}

pub(super) fn verify_same_directory(
    first: &SecureDirectory,
    second: &SecureDirectory,
) -> Result<(), super::Failure> {
    let first = directory(first)?;
    let second = directory(second)?;
    verify_directory_entry(&first)?;
    verify_directory_entry(&second)?;
    if first.identity == second.identity {
        Ok(())
    } else {
        Err(super::Failure::Unsafe)
    }
}

pub(super) struct PrivateLease {
    directory: SecureDirectory,
    file: File,
    name: OsString,
    identity: NativeIdentity,
}
impl PrivateLease {
    pub(super) fn verify(&self) -> Result<(), super::Failure> {
        let directory = directory(&self.directory)?;
        verify_directory_entry(&directory)?;
        verify_acl(&directory.file)?;
        if private_file_identity(&self.file.metadata().map_err(classify)?)? != self.identity
            || file_identity_at(&directory.file, &self.name)? != Some(self.identity)
        {
            return Err(super::Failure::Unsafe);
        }
        verify_acl(&self.file).map_err(Into::into)
    }
}
pub(super) fn private_lease(
    handle: &SecureDirectory,
    name: &OsStr,
) -> Result<PrivateLease, super::Failure> {
    let directory = directory(handle)?;
    verify_directory_entry(&directory)?;
    let file = match open_file_at(&directory.file, name, libc::O_RDWR | libc::O_NONBLOCK, 0) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            match open_file_at(
                &directory.file,
                name,
                libc::O_RDWR | libc::O_NONBLOCK | libc::O_CREAT | libc::O_EXCL,
                PRIVATE_FILE_MODE,
            ) {
                Ok(file) => {
                    set_file_mode(&file, PRIVATE_FILE_MODE).map_err(classify)?;
                    file.sync_all()
                        .and_then(|()| directory.file.sync_all())
                        .map_err(|_| super::Failure::Indeterminate)?;
                    file
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    open_file_at(&directory.file, name, libc::O_RDWR | libc::O_NONBLOCK, 0)
                        .map_err(classify)?
                }
                Err(error) => return Err(classify(error).into()),
            }
        }
        Err(error) => return Err(classify(error).into()),
    };
    let identity = private_file_identity(&file.metadata().map_err(classify)?)?;
    verify_acl(&file)?;
    file.try_lock().map_err(|error| match error {
        std::fs::TryLockError::WouldBlock => super::Failure::Busy,
        std::fs::TryLockError::Error(_) => super::Failure::Unavailable,
    })?;
    let lease = PrivateLease {
        directory: handle.clone(),
        file,
        name: name.to_owned(),
        identity,
    };
    lease.verify()?;
    Ok(lease)
}

// Recover only the original admitted publication. Never treat a repeated fsync as
// proof of its old durability. Copy validated exact bytes into a new synchronized
// inode and publish a new checkpoint for the retained original nonce.
pub(super) fn reconcile_publication(
    path: &Path,
    name: &OsStr,
    maximum: usize,
    validate: impl FnOnce(&[u8]) -> Result<(), super::Failure>,
) -> Result<(), super::Failure> {
    let (handle, authority) = admit_existing(path)?.ok_or(super::Failure::Missing)?;
    authority.verify()?;
    reconcile_retained_publication(&handle, name, maximum, validate)?;
    authority.verify()
}

pub(super) fn reconcile_retained_publication(
    handle: &SecureDirectory,
    name: &OsStr,
    maximum: usize,
    validate: impl FnOnce(&[u8]) -> Result<(), super::Failure>,
) -> Result<(), super::Failure> {
    let directory = directory(handle)?;
    match open_mutation(&directory.file, OsStr::new(MUTATION_NAME), false) {
        Ok(_) => return Err(super::Failure::Conflict),
        Err(super::Failure::Indeterminate) => {}
        Err(error) => return Err(error),
    }
    verify_directory_entry(&directory)?;
    verify_acl(&directory.file)?;
    let mut guard = original_phase(&directory.file, OsStr::new(MUTATION_NAME))?;
    let phase = read_phase(&guard.file)?;
    if phase[5] != 1 || phase[6] != 1 || phase[24..56] != Sha256::digest(name.as_bytes())[..] {
        return Err(super::Failure::Indeterminate);
    }
    if phase[4] == 2 && phase[7] != 1 {
        // A durable pre-effect or proven-CAS-conflict phase has no publication
        // to replay. Closing that original phase cannot issue a new action.
        guard.finish()?;
        return Ok(());
    }
    let current = read_unfenced(&directory, name, maximum)?;
    if current
        .as_ref()
        .is_some_and(|snapshot| snapshot.bytes.len() > maximum)
    {
        return Err(super::Failure::LimitExceeded);
    }
    let proposal = if phase[4] == 2 {
        read_unfenced(&directory, OsStr::new(PROPOSAL_NAME), PROPOSAL_LIMIT)?
    } else {
        None
    };
    let mut candidate = None;
    if let Some(proposal) = &proposal {
        let payload = &proposal.bytes;
        if payload.len() < PROPOSAL_HEADER
            || payload.len() > PROPOSAL_LIMIT
            || payload[..5] != *b"STPR\x01"
            || payload[5..21] != phase[8..24]
            || payload[27] > 1
        {
            return Err(super::Failure::Indeterminate);
        }
        let name_bytes = u16::from_be_bytes(
            payload[21..23]
                .try_into()
                .map_err(|_| super::Failure::Unsafe)?,
        ) as usize;
        let body_bytes = u32::from_be_bytes(
            payload[23..27]
                .try_into()
                .map_err(|_| super::Failure::Unsafe)?,
        ) as usize;
        if name_bytes > 255
            || body_bytes > maximum
            || payload.len() != PROPOSAL_HEADER + name_bytes + body_bytes
            || payload[PROPOSAL_HEADER..PROPOSAL_HEADER + name_bytes] != *name.as_bytes()
        {
            return Err(super::Failure::Indeterminate);
        }
        let bytes = &payload[PROPOSAL_HEADER + name_bytes..];
        if phase[56..88] != fingerprint(bytes) {
            return Err(super::Failure::Indeterminate);
        }
        let already_published = current
            .as_ref()
            .is_some_and(|snapshot| snapshot.bytes.as_slice() == bytes);
        let original_base = match &current {
            None => payload[27] == 0 && payload[28..60] == [0; 32],
            Some(snapshot) => {
                payload[27] == 1
                    && payload[28..60]
                        == snapshot_digest(file_snapshot_identity(&snapshot.identity)?)
            }
        };
        if !already_published && !original_base {
            return Err(super::Failure::Indeterminate);
        }
        candidate = Some(bytes);
    }
    let bytes = match candidate {
        Some(bytes) => bytes,
        None => {
            let current = current.as_ref().ok_or(super::Failure::Indeterminate)?;
            if phase[56..88] != fingerprint(&current.bytes) {
                return Err(super::Failure::Indeterminate);
            }
            current.bytes.as_slice()
        }
    };
    validate(bytes)?;
    let mut nonce = [0; 16];
    getrandom::fill(&mut nonce).map_err(|_| super::Failure::Unavailable)?;
    let prepared = UnixSecureFilesystem.prepare_private_file(handle, name, bytes, nonce)?;
    verify_prepared_acl(&prepared)?;
    let committed = UnixSecureFilesystem.commit_private_file(
        prepared,
        current.as_ref().map(|snapshot| &snapshot.identity),
    )?;
    if committed.outcome != SecureCommitOutcome::Committed {
        return Err(super::Failure::Indeterminate);
    }
    guard.finish()?;
    verify_directory_entry(&directory).map_err(Into::into)
}

fn removal_digest(expected: &NativeFileIdentity) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(expected.fingerprint);
    digest.update(expected.version.entry.device.to_be_bytes());
    digest.update(expected.version.entry.inode.to_be_bytes());
    digest.update(expected.version.size.to_be_bytes());
    digest.update(expected.version.modified_seconds.to_be_bytes());
    digest.update(expected.version.modified_nanoseconds.to_be_bytes());
    digest.finalize().into()
}
fn original_phase(parent: &File, name: &OsStr) -> Result<MutationGuard, super::Failure> {
    let file = open_file_at(parent, name, libc::O_RDWR | libc::O_NONBLOCK, 0).map_err(classify)?;
    let identity = private_file_identity(&file.metadata().map_err(classify)?)?;
    file.lock().map_err(classify)?;
    let guard = MutationGuard {
        file,
        parent: parent.try_clone().map_err(classify)?,
        name: name.to_owned(),
        identity,
    };
    guard.check()?;
    Ok(guard)
}

// This is explicit current-directory admission, not proof that old file writes
// survived. Missing children are never created by recovery. Each existing child
// is checked under the same parent descriptor that owns its original fence.
fn validate_initialization_phase(
    guard: &MutationGuard,
    parent: &File,
    name: &OsStr,
) -> Result<(), super::Failure> {
    let phase = read_phase(&guard.file)?;
    if phase[4] != 1
        || phase[5] != 1
        || phase[6] != 3
        || phase[24..56] != Sha256::digest(name.as_bytes())[..]
        || phase[56..88] != [0; 32]
    {
        return Err(super::Failure::Indeterminate);
    }
    if parent.metadata().map_err(classify)?.uid() != effective_user_id() {
        return Err(super::Failure::Unsafe);
    }
    Ok(())
}

pub(super) fn reconcile_initialization(path: &Path) -> Result<(), super::Failure> {
    if !path.is_absolute() {
        return Err(super::Failure::Unsafe);
    }
    let mut parent = File::open("/").map_err(classify)?;
    for component in path.components() {
        let Component::Normal(name) = component else {
            if matches!(component, Component::RootDir) {
                continue;
            }
            return Err(super::Failure::Unsafe);
        };
        let file = match open_directory_at(&parent, name) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match open_mutation(&parent, &initialization_name(name), false) {
                    Ok(_) => {}
                    Err(super::Failure::Indeterminate) => {
                        let mut guard = original_phase(&parent, &initialization_name(name))?;
                        validate_initialization_phase(&guard, &parent, name)?;
                        // An original pre-mkdir interruption has no child to admit.
                        // Publish fresh no-effect completion, without creating one.
                        if file_identity_at(&parent, name)?.is_some() {
                            return Err(super::Failure::Unsafe);
                        }
                        parent
                            .sync_all()
                            .map_err(|_| super::Failure::Indeterminate)?;
                        guard.finish()?;
                        if file_identity_at(&parent, name)?.is_some() {
                            return Err(super::Failure::Unsafe);
                        }
                    }
                    Err(error) => return Err(error),
                }
                return Ok(());
            }
            Err(error) => return Err(classify(error).into()),
        };
        match open_mutation(&parent, &initialization_name(name), false) {
            Ok(_) => {}
            Err(super::Failure::Indeterminate) => {
                let mut guard = original_phase(&parent, &initialization_name(name))?;
                validate_initialization_phase(&guard, &parent, name)?;
                let identity = private_directory_identity(&file).map_err(classify)?;
                verify_acl(&file)?;
                if metadata_identity(
                    &open_directory_at(&parent, name)
                        .map_err(classify)?
                        .metadata()
                        .map_err(classify)?,
                ) != identity
                {
                    return Err(super::Failure::Unsafe);
                }
                file.sync_all()
                    .and_then(|()| parent.sync_all())
                    .map_err(|_| super::Failure::Indeterminate)?;
                guard.finish()?;
                if metadata_identity(
                    &open_directory_at(&parent, name)
                        .map_err(classify)?
                        .metadata()
                        .map_err(classify)?,
                ) != identity
                {
                    return Err(super::Failure::Unsafe);
                }
            }
            Err(error) => return Err(error),
        }
        parent = file;
    }
    Ok(())
}

pub(super) fn reconcile_removal(
    handle: &SecureDirectory,
    name: &OsStr,
    maximum: usize,
    validate: impl FnOnce(&[u8]) -> Result<(), super::Failure>,
) -> Result<(), super::Failure> {
    let directory = directory(handle)?;
    verify_directory_entry(&directory)?;
    verify_acl(&directory.file)?;
    let mut guard = original_phase(&directory.file, OsStr::new(MUTATION_NAME))?;
    let phase = read_phase(&guard.file)?;
    if phase[5] != 1
        || phase[6] != 2
        || phase[24..56] != Sha256::digest(name.as_bytes())[..]
        || phase[56..88] == [0; 32]
    {
        return Err(super::Failure::Conflict);
    }
    let current = read_unfenced(&directory, name, maximum)?;
    if let Some(current) = current {
        if current.bytes.len() > maximum {
            return Err(super::Failure::LimitExceeded);
        }
        if phase[56..88] != removal_digest(file_snapshot_identity(&current.identity)?) {
            return Err(super::Failure::Conflict);
        }
        validate(&current.bytes)?;
        remove_and_sync(handle, name, &current.identity)?;
    }
    // A new synchronized checkpoint records validated current absence. It does
    // not infer success merely from the old uncertain directory synchronization.
    if file_version_at(&directory.file, name)?.is_some() {
        return Err(super::Failure::Conflict);
    }
    guard.finish()?;
    verify_directory_entry(&directory).map_err(Into::into)
}

pub(super) fn read_for_recovery(
    handle: &SecureDirectory,
    name: &OsStr,
    maximum: usize,
) -> Result<Option<super::Snapshot>, super::Failure> {
    let directory = directory(handle)?;
    let guard = original_phase(&directory.file, OsStr::new(MUTATION_NAME))?;
    read_phase(&guard.file)?;
    verify_directory_entry(&directory)?;
    verify_acl(&directory.file)?;
    let value = read_unfenced(&directory, name, maximum)?;
    if value
        .as_ref()
        .is_some_and(|snapshot| snapshot.bytes.len() > maximum)
    {
        return Err(super::Failure::LimitExceeded);
    }
    guard.check()?;
    Ok(value)
}
