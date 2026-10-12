//! One retained bounded record binding used by immutable reads and fresh replacements.

use super::{NativeRegistrationStore, records::classify};
use crate::native_deployment::contracts::NativeFailure;
use sha2::{Digest, Sha256};
use std::{
    ffi::CString,
    fs::File,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::{FileExt, MetadataExt},
    },
};

pub(super) struct BoundRecord {
    pub(super) file: File,
    pub(super) name: CString,
    metadata: std::fs::Metadata,
    pub(super) sha256: [u8; 32],
}
impl BoundRecord {
    pub(super) fn open(
        store: &NativeRegistrationStore,
        parent: &File,
        name: &str,
        maximum: usize,
    ) -> Result<(Self, Vec<u8>), NativeFailure> {
        let name = CString::new(name).map_err(|_| NativeFailure::InvalidRecord)?;
        // SAFETY: retained protected parent and a native fixed single component, without following links.
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            )
        };
        if fd < 0 {
            return Err(NativeFailure::IoUnavailable);
        }
        // SAFETY: successful open returned a newly owned read-only descriptor.
        let file = unsafe { File::from_raw_fd(fd) };
        Self::retain_file(store, parent, file, name, maximum)
    }
    pub(super) fn retain_file(
        store: &NativeRegistrationStore,
        parent: &File,
        file: File,
        name: CString,
        maximum: usize,
    ) -> Result<(Self, Vec<u8>), NativeFailure> {
        store
            .protection
            .verify_regular_file(&file)
            .map_err(classify)?;
        store.protection.verify_custody(&[parent, &file])?;
        let metadata = file.metadata().map_err(|_| NativeFailure::IoUnavailable)?;
        if !metadata.is_file() || metadata.nlink() != 1 || metadata.len() > maximum as u64 {
            return Err(NativeFailure::UnsafeNativeState);
        }
        let bytes = read_bytes(&file, metadata.len() as usize)?;
        let record = Self {
            file,
            name,
            metadata,
            sha256: Sha256::digest(&bytes).into(),
        };
        record.verify(store, parent)?;
        Ok((record, bytes))
    }
    pub(super) fn verify(
        &self,
        store: &NativeRegistrationStore,
        parent: &File,
    ) -> Result<(), NativeFailure> {
        store
            .protection
            .verify_regular_file(&self.file)
            .map_err(classify)?;
        store.protection.verify_custody(&[parent, &self.file])?;
        let current = self
            .file
            .metadata()
            .map_err(|_| NativeFailure::IoUnavailable)?;
        let original = &self.metadata;
        if !current.is_file()
            || current.nlink() != 1
            || current.dev() != original.dev()
            || current.ino() != original.ino()
            || current.len() != original.len()
            || current.mode() != original.mode()
            || current.uid() != original.uid()
            || current.gid() != original.gid()
            || current.mtime() != original.mtime()
            || current.mtime_nsec() != original.mtime_nsec()
            || current.ctime() != original.ctime()
            || current.ctime_nsec() != original.ctime_nsec()
        {
            return Err(NativeFailure::UnsafeNativeState);
        }
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: retained protected directory, checked component, initialized only on success.
        if unsafe {
            libc::fstatat(
                parent.as_raw_fd(),
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
        if device != original.dev() || stat.st_ino != original.ino() {
            return Err(NativeFailure::UnsafeNativeState);
        }
        let observed: [u8; 32] =
            Sha256::digest(read_bytes(&self.file, original.len() as usize)?).into();
        if observed != self.sha256 {
            return Err(NativeFailure::UnsafeNativeState);
        }
        Ok(())
    }
}

fn read_bytes(file: &File, size: usize) -> Result<Vec<u8>, NativeFailure> {
    if size > 64 * 1024 {
        return Err(NativeFailure::LimitExceeded);
    }
    let mut bytes = vec![0; size];
    let mut offset = 0;
    for _ in 0..16 {
        if offset == bytes.len() {
            return Ok(bytes);
        }
        match file.read_at(&mut bytes[offset..], offset as u64) {
            Ok(0) => return Err(NativeFailure::InvalidRecord),
            Ok(count) => offset += count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => (),
            Err(_) => return Err(NativeFailure::IoUnavailable),
        }
    }
    if offset == bytes.len() {
        Ok(bytes)
    } else {
        Err(NativeFailure::IoUnavailable)
    }
}
