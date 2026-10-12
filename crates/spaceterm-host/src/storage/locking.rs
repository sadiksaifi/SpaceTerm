//! SQLite's rollback-journal POSIX byte-range protocol, with process-local owners.
//!
//! All connections to an inode share one retained descriptor. Authority inspection
//! uses fstat/fstatat, never another open/close of the database inode. The registry
//! therefore cannot accidentally release another connection's fcntl locks.

use std::collections::HashSet;
use std::os::fd::RawFd;

use rusqlite::ffi;

use super::directory::{Entry, mutex};

const PENDING_BYTE: libc::off_t = 0x4000_0000;
const RESERVED_BYTE: libc::off_t = PENDING_BYTE + 1;
const SHARED_FIRST: libc::off_t = PENDING_BYTE + 2;
const SHARED_SIZE: libc::off_t = 510;

#[derive(Default)]
pub(super) struct Locks {
    readers: HashSet<u64>,
    reserved: Option<u64>,
    pending: Option<u64>,
    exclusive: Option<u64>,
}

fn set(fd: RawFd, kind: libc::c_short, start: libc::off_t, length: libc::off_t) -> i32 {
    // SAFETY: lock has the platform's initialized flock layout, and fd is retained.
    let mut lock: libc::flock = unsafe { std::mem::zeroed() };
    lock.l_type = kind;
    lock.l_whence = libc::SEEK_SET as libc::c_short;
    lock.l_start = start;
    lock.l_len = length;
    if unsafe { libc::fcntl(fd, libc::F_SETLK, &lock) } == 0 {
        ffi::SQLITE_OK
    } else if matches!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::EACCES | libc::EAGAIN)
    ) {
        ffi::SQLITE_BUSY
    } else {
        ffi::SQLITE_IOERR_LOCK
    }
}

impl Entry {
    pub(super) fn lock(&self, owner: u64, requested: i32, level: &mut i32) -> i32 {
        let mut locks = mutex(&self.locks);
        if requested <= *level {
            return ffi::SQLITE_OK;
        }
        if *level == ffi::SQLITE_LOCK_NONE {
            if requested != ffi::SQLITE_LOCK_SHARED {
                return ffi::SQLITE_IOERR_LOCK;
            }
            if locks.pending.is_some() || locks.exclusive.is_some() {
                return ffi::SQLITE_BUSY;
            }
            if locks.readers.is_empty() {
                let rc = set(self.fd(), libc::F_RDLCK as _, PENDING_BYTE, 1);
                if rc != ffi::SQLITE_OK {
                    return rc;
                }
                let rc = set(self.fd(), libc::F_RDLCK as _, SHARED_FIRST, SHARED_SIZE);
                let released = set(self.fd(), libc::F_UNLCK as _, PENDING_BYTE, 1);
                if rc != ffi::SQLITE_OK {
                    return rc;
                }
                if released != ffi::SQLITE_OK {
                    return released;
                }
            }
            locks.readers.insert(owner);
            *level = ffi::SQLITE_LOCK_SHARED;
            return ffi::SQLITE_OK;
        }
        if requested == ffi::SQLITE_LOCK_RESERVED {
            if locks.reserved.is_some_and(|value| value != owner) {
                return ffi::SQLITE_BUSY;
            }
            let rc = set(self.fd(), libc::F_WRLCK as _, RESERVED_BYTE, 1);
            if rc != ffi::SQLITE_OK {
                return rc;
            }
            locks.reserved = Some(owner);
            *level = requested;
            return ffi::SQLITE_OK;
        }
        if !matches!(
            requested,
            ffi::SQLITE_LOCK_PENDING | ffi::SQLITE_LOCK_EXCLUSIVE
        ) {
            return ffi::SQLITE_IOERR_LOCK;
        }
        if locks.pending.is_some_and(|value| value != owner)
            || locks.exclusive.is_some_and(|value| value != owner)
            || locks.reserved.is_some_and(|value| value != owner)
        {
            return ffi::SQLITE_BUSY;
        }
        if locks.pending.is_none() {
            let rc = set(self.fd(), libc::F_WRLCK as _, PENDING_BYTE, 1);
            if rc != ffi::SQLITE_OK {
                return rc;
            }
            locks.pending = Some(owner);
            *level = ffi::SQLITE_LOCK_PENDING;
        }
        if requested == ffi::SQLITE_LOCK_PENDING {
            return ffi::SQLITE_OK;
        }
        if locks.readers.len() != 1 {
            return ffi::SQLITE_BUSY;
        }
        let rc = set(self.fd(), libc::F_WRLCK as _, SHARED_FIRST, SHARED_SIZE);
        if rc == ffi::SQLITE_OK {
            locks.exclusive = Some(owner);
            *level = ffi::SQLITE_LOCK_EXCLUSIVE;
        }
        rc
    }

    pub(super) fn unlock(&self, owner: u64, requested: i32, level: &mut i32) -> i32 {
        if requested >= *level {
            return ffi::SQLITE_OK;
        }
        if !matches!(requested, ffi::SQLITE_LOCK_NONE | ffi::SQLITE_LOCK_SHARED) {
            return ffi::SQLITE_IOERR_UNLOCK;
        }
        let mut locks = mutex(&self.locks);
        if locks.exclusive == Some(owner) {
            let kind = if requested == ffi::SQLITE_LOCK_SHARED {
                libc::F_RDLCK
            } else {
                libc::F_UNLCK
            };
            if set(self.fd(), kind as _, SHARED_FIRST, SHARED_SIZE) != ffi::SQLITE_OK {
                return ffi::SQLITE_IOERR_UNLOCK;
            }
            locks.exclusive = None;
        } else if requested == ffi::SQLITE_LOCK_NONE
            && locks.readers.len() == 1
            && set(self.fd(), libc::F_UNLCK as _, SHARED_FIRST, SHARED_SIZE) != ffi::SQLITE_OK
        {
            return ffi::SQLITE_IOERR_UNLOCK;
        }
        if locks.pending == Some(owner) {
            if set(self.fd(), libc::F_UNLCK as _, PENDING_BYTE, 1) != ffi::SQLITE_OK {
                return ffi::SQLITE_IOERR_UNLOCK;
            }
            locks.pending = None;
        }
        if locks.reserved == Some(owner) {
            if set(self.fd(), libc::F_UNLCK as _, RESERVED_BYTE, 1) != ffi::SQLITE_OK {
                return ffi::SQLITE_IOERR_UNLOCK;
            }
            locks.reserved = None;
        }
        if requested == ffi::SQLITE_LOCK_NONE {
            locks.readers.remove(&owner);
        }
        *level = requested;
        ffi::SQLITE_OK
    }

    pub(super) fn reserved(&self) -> Result<bool, i32> {
        let locks = mutex(&self.locks);
        if locks.reserved.is_some() || locks.pending.is_some() || locks.exclusive.is_some() {
            return Ok(true);
        }
        // SAFETY: fcntl initializes the lock result; fd is retained for this call.
        let mut lock: libc::flock = unsafe { std::mem::zeroed() };
        lock.l_type = libc::F_WRLCK as _;
        lock.l_whence = libc::SEEK_SET as _;
        lock.l_start = RESERVED_BYTE;
        lock.l_len = 1;
        if unsafe { libc::fcntl(self.fd(), libc::F_GETLK, &mut lock) } != 0 {
            return Err(ffi::SQLITE_IOERR_CHECKRESERVEDLOCK);
        }
        Ok(lock.l_type != libc::F_UNLCK as libc::c_short)
    }
}
