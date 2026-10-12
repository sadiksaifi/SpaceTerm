use std::ffi::CStr;
use std::fs::File;
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd};

use super::{MAX_DRAIN_CALLS, MAX_EVENTS, MAX_HANDLES, WorkBudget};
use crate::protected_directory::ProtectedDirectoryError;

const MUTATIONS: u32 = libc::IN_MODIFY
    | libc::IN_ATTRIB
    | libc::IN_CLOSE_WRITE
    | libc::IN_MOVED_FROM
    | libc::IN_MOVED_TO
    | libc::IN_CREATE
    | libc::IN_DELETE
    | libc::IN_DELETE_SELF
    | libc::IN_MOVE_SELF;
const LOSSES: u32 = libc::IN_UNMOUNT | libc::IN_IGNORED;
const HEADER_BYTES: usize = 16;
const BUFFER_BYTES: usize = 4096;
const MAX_NAME_BYTES: usize = 256;

struct Watch {
    descriptor: libc::c_int,
    directory: bool,
}

pub(super) struct Observation {
    queue: File,
    proc_directory: File,
    watches: Vec<Watch>,
    changed: bool,
}

impl Observation {
    pub(super) fn new(work: &mut WorkBudget) -> Result<Self, ProtectedDirectoryError> {
        let mut watches = Vec::new();
        watches
            .try_reserve_exact(MAX_HANDLES)
            .map_err(|_| ProtectedDirectoryError::Unavailable)?;
        let descriptor = work.call(|| {
            #[cfg(test)]
            super::testing::creation()?;
            // SAFETY: inotify_init1 takes flags only and returns an owned descriptor.
            let descriptor = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
            if descriptor < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(descriptor)
            }
        })?;
        // SAFETY: the successful init returned a new descriptor owned here.
        let queue = unsafe { File::from_raw_fd(descriptor) };
        let descriptor = work.call(|| {
            // SAFETY: this fixed native path is NUL-terminated. It selects the
            // host proc descriptor directory, never a caller's original path.
            let descriptor = unsafe {
                libc::open(
                    c"/proc/self/fd".as_ptr(),
                    libc::O_RDONLY
                        | libc::O_DIRECTORY
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC
                        | libc::O_NONBLOCK,
                )
            };
            if descriptor < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(descriptor)
            }
        })?;
        // SAFETY: open returned a new descriptor owned here.
        let proc_directory = unsafe { File::from_raw_fd(descriptor) };
        let mut filesystem = MaybeUninit::<libc::statfs>::uninit();
        work.call(|| {
            // SAFETY: the retained proc descriptor and output storage are live.
            if unsafe { libc::fstatfs(proc_directory.as_raw_fd(), filesystem.as_mut_ptr()) } != 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        })?;
        // SAFETY: fstatfs initialized the structure on success.
        let filesystem = unsafe { filesystem.assume_init() };
        if filesystem.f_type as u32 != libc::PROC_SUPER_MAGIC as u32 {
            return Err(ProtectedDirectoryError::NativeUnavailable);
        }
        Ok(Self {
            queue,
            proc_directory,
            watches,
            changed: false,
        })
    }

    pub(super) fn enroll(
        &mut self,
        held: &File,
        work: &mut WorkBudget,
    ) -> Result<(), ProtectedDirectoryError> {
        if self.watches.len() == MAX_HANDLES {
            return Err(ProtectedDirectoryError::Unavailable);
        }
        let mut path = [0; 32];
        let reference = descriptor_reference(held, &mut path)?;
        let descriptor = work.call(|| {
            #[cfg(test)]
            super::testing::enrollment()?;
            // SAFETY: reference is NUL-terminated and both descriptors are live.
            // IN_DONT_FOLLOW would watch the proc link instead of the held inode.
            // IN_MASK_ADD preserves an existing mark and its pending events when
            // two retained descriptors happen to select the same inode.
            let descriptor = unsafe {
                libc::inotify_add_watch(
                    self.queue.as_raw_fd(),
                    reference.as_ptr(),
                    MUTATIONS | libc::IN_MASK_ADD,
                )
            };
            if descriptor < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(descriptor)
            }
        })?;
        let status = self.require_reference(held, reference, work)?;
        if !self
            .watches
            .iter()
            .any(|watch| watch.descriptor == descriptor)
        {
            self.watches.push(Watch {
                descriptor,
                directory: status.st_mode & libc::S_IFMT == libc::S_IFDIR,
            });
        }
        Ok(())
    }

    fn require_reference(
        &self,
        held: &File,
        reference: &CStr,
        work: &mut WorkBudget,
    ) -> Result<libc::stat, ProtectedDirectoryError> {
        let held_status = status_of(held, work)?;
        let resolved = status_at_path(reference, work)?;
        let retained_proc = status_of(&self.proc_directory, work)?;
        let current_proc = status_at_path(c"/proc/self/fd", work)?;
        if !same_object(&held_status, &resolved) || !same_object(&retained_proc, &current_proc) {
            return Err(ProtectedDirectoryError::Changed);
        }
        // proc_fd_link selects the held struct file's f_path. The outer guard
        // owns this duplicate until queue teardown, so its number cannot be reused.
        // These checks require a trusted host procfs and mount namespace; they do
        // not authenticate a hostile namespace or exclude transient over-mounts.
        Ok(held_status)
    }

    pub(super) fn finish(&mut self, work: &mut WorkBudget) -> Result<(), ProtectedDirectoryError> {
        let mut buffer = [0; BUFFER_BYTES];
        let mut observed = 0;
        for _ in 0..MAX_DRAIN_CALLS {
            let Some(count) = work.retry(|| self.read_batch(&mut buffer))? else {
                return if self.changed {
                    Err(ProtectedDirectoryError::Changed)
                } else {
                    Ok(())
                };
            };
            if count == 0 || count > buffer.len() {
                return Err(ProtectedDirectoryError::Unavailable);
            }
            self.inspect_records(&buffer[..count], &mut observed)?;
        }
        Err(ProtectedDirectoryError::Unavailable)
    }

    fn inspect_records(
        &mut self,
        mut records: &[u8],
        observed: &mut usize,
    ) -> Result<(), ProtectedDirectoryError> {
        while !records.is_empty() {
            if records.len() < HEADER_BYTES || *observed == MAX_EVENTS {
                return Err(ProtectedDirectoryError::Unavailable);
            }
            *observed += 1;
            let descriptor = i32::from_ne_bytes(word(records, 0)?.to_ne_bytes());
            let mask = word(records, 4)?;
            let cookie = word(records, 8)?;
            let length = usize::try_from(word(records, 12)?)
                .map_err(|_| ProtectedDirectoryError::Unavailable)?;
            if length > MAX_NAME_BYTES || !length.is_multiple_of(4) {
                return Err(ProtectedDirectoryError::Unavailable);
            }
            let end = HEADER_BYTES
                .checked_add(length)
                .ok_or(ProtectedDirectoryError::Unavailable)?;
            let name = records
                .get(HEADER_BYTES..end)
                .ok_or(ProtectedDirectoryError::Unavailable)?;
            if !name.is_empty() {
                let nul = name
                    .iter()
                    .position(|byte| *byte == 0)
                    .ok_or(ProtectedDirectoryError::Unavailable)?;
                if nul == 0
                    || name[..nul].contains(&b'/')
                    || name[nul..].iter().any(|byte| *byte != 0)
                {
                    return Err(ProtectedDirectoryError::Unavailable);
                }
            }
            if mask == libc::IN_Q_OVERFLOW {
                // Loss of history is never an empty successful interval.
                return Err(ProtectedDirectoryError::Unavailable);
            }
            let watch = self
                .watches
                .iter()
                .find(|watch| watch.descriptor == descriptor)
                .ok_or(ProtectedDirectoryError::Unavailable)?;
            if mask & !(MUTATIONS | LOSSES | libc::IN_ISDIR) != 0
                || mask & (MUTATIONS | LOSSES) == 0
                || (!watch.directory && (length != 0 || mask & libc::IN_ISDIR != 0))
                || (mask
                    & (libc::IN_CREATE | libc::IN_DELETE | libc::IN_MOVED_FROM | libc::IN_MOVED_TO)
                    != 0
                    && length == 0)
                || (cookie != 0 && mask & (libc::IN_MOVED_FROM | libc::IN_MOVED_TO) == 0)
            {
                return Err(ProtectedDirectoryError::Unavailable);
            }
            // Parent namespace events are intentionally not name-filtered. A
            // restored binding must not disappear from this operation's history.
            self.changed = true;
            records = &records[end..];
        }
        Ok(())
    }

    fn read_batch(&self, buffer: &mut [u8; BUFFER_BYTES]) -> io::Result<Option<usize>> {
        #[cfg(test)]
        if let Some(count) = self.inject_read(buffer)? {
            return Ok(Some(count));
        }
        // SAFETY: queue is owned and buffer has BUFFER_BYTES writable bytes. The
        // inotify descriptor is nonblocking, so EAGAIN marks the end of this drain.
        let count = unsafe {
            libc::read(
                self.queue.as_raw_fd(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
            )
        };
        if count < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::WouldBlock {
                Ok(None)
            } else {
                Err(error)
            }
        } else {
            usize::try_from(count)
                .map(Some)
                .map_err(|_| io::Error::from_raw_os_error(libc::EIO))
        }
    }

    #[cfg(test)]
    fn inject_read(&self, buffer: &mut [u8; BUFFER_BYTES]) -> io::Result<Option<usize>> {
        use super::testing::{Fault, fault};

        let Some(fault) = fault() else {
            return Ok(None);
        };
        let (descriptor, mask, length) = match fault {
            Fault::QueueRead => return Err(io::Error::from_raw_os_error(libc::EIO)),
            Fault::InterruptedDrain => return Err(io::Error::from_raw_os_error(libc::EINTR)),
            Fault::WatchLoss => (self.watches[0].descriptor, libc::IN_IGNORED, 0_u32),
            Fault::QueueOverflow => (-1, libc::IN_Q_OVERFLOW, 0_u32),
            Fault::MalformedRecord => (self.watches[0].descriptor, libc::IN_ATTRIB, 4_u32),
            Fault::UnboundedDrain => (self.watches[0].descriptor, libc::IN_ATTRIB, 0_u32),
            _ => return Ok(None),
        };
        buffer[..4].copy_from_slice(&descriptor.to_ne_bytes());
        buffer[4..8].copy_from_slice(&mask.to_ne_bytes());
        buffer[8..12].copy_from_slice(&0_u32.to_ne_bytes());
        buffer[12..16].copy_from_slice(&length.to_ne_bytes());
        // The malformed record advertises a name absent from the returned batch.
        // A one-shot loss is followed by a real drain to exercise sticky rejection.
        if fault != Fault::UnboundedDrain {
            let _ = super::testing::take_fault();
        }
        Ok(Some(HEADER_BYTES))
    }
}

fn descriptor_reference<'buffer>(
    held: &File,
    buffer: &'buffer mut [u8; 32],
) -> Result<&'buffer CStr, ProtectedDirectoryError> {
    let prefix = b"/proc/self/fd/";
    buffer[..prefix.len()].copy_from_slice(prefix);
    let mut descriptor =
        u32::try_from(held.as_raw_fd()).map_err(|_| ProtectedDirectoryError::Unavailable)?;
    let mut digits = [0; 10];
    let mut count = 0;
    loop {
        digits[count] = b'0' + (descriptor % 10) as u8;
        count += 1;
        descriptor /= 10;
        if descriptor == 0 {
            break;
        }
    }
    for (index, digit) in digits[..count].iter().rev().enumerate() {
        buffer[prefix.len() + index] = *digit;
    }
    let end = prefix.len() + count;
    buffer[end] = 0;
    CStr::from_bytes_with_nul(&buffer[..=end]).map_err(|_| ProtectedDirectoryError::Unavailable)
}

fn status_of(file: &File, work: &mut WorkBudget) -> Result<libc::stat, ProtectedDirectoryError> {
    let mut status = MaybeUninit::<libc::stat>::uninit();
    work.call(|| {
        // SAFETY: file is borrowed live and status has writable native storage.
        if unsafe { libc::fstat(file.as_raw_fd(), status.as_mut_ptr()) } != 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    })?;
    // SAFETY: fstat initialized status on success.
    Ok(unsafe { status.assume_init() })
}

fn status_at_path(
    path: &CStr,
    work: &mut WorkBudget,
) -> Result<libc::stat, ProtectedDirectoryError> {
    let mut status = MaybeUninit::<libc::stat>::uninit();
    work.call(|| {
        // SAFETY: the internal proc path is NUL-terminated, with writable storage.
        // Following it is required to inspect the retained descriptor's object.
        if unsafe { libc::stat(path.as_ptr(), status.as_mut_ptr()) } != 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    })?;
    // SAFETY: stat initialized status on success.
    Ok(unsafe { status.assume_init() })
}

fn same_object(left: &libc::stat, right: &libc::stat) -> bool {
    left.st_dev == right.st_dev
        && left.st_ino == right.st_ino
        && left.st_mode & libc::S_IFMT == right.st_mode & libc::S_IFMT
}

fn word(record: &[u8], offset: usize) -> Result<u32, ProtectedDirectoryError> {
    let bytes = record
        .get(offset..offset + 4)
        .ok_or(ProtectedDirectoryError::Unavailable)?;
    let bytes = <[u8; 4]>::try_from(bytes).map_err(|_| ProtectedDirectoryError::Unavailable)?;
    Ok(u32::from_ne_bytes(bytes))
}

// Linux 5.15 inotify covers completed notify_change/xattr, namespace and ordinary
// write paths. It does not report mmap/msync/munmap writes, fileattr flag changes,
// or over-mounts. A drained queue is neither a mutation barrier nor proof that the
// returned bytes or ACL state remained unchanged throughout an operation.
