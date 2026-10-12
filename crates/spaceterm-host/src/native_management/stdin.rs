//! Private stdin request transport preserved by sudo and pkexec.

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

use crate::native_deployment::contracts::NativeFailure;

use super::helper_lifetime::HelperDeadline;
use super::request::{ManagementRequest, REQUEST_BYTES};

/// Own the manager's fixed stdin pipe. Descriptor data never supplies authority.
pub struct RequestStdin {
    pipe: OwnedFd,
}
impl RequestStdin {
    /// Claim stdin at one-request manager entry, before any subsequent child.
    ///
    /// The composition root must not have created another stdin reader. Only a
    /// read-only pipe is accepted. Both the original and retained descriptor are
    /// CLOEXEC; the original is then closed so request channels cannot leak into
    /// sudo, supervisor, credential, or Terminal Session children.
    pub fn claim() -> Result<Self, NativeFailure> {
        let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: fstat initializes status only on success, with no pointer escape.
        if unsafe { libc::fstat(libc::STDIN_FILENO, status.as_mut_ptr()) } != 0 {
            return Err(NativeFailure::IoUnavailable);
        }
        // SAFETY: successful fstat initialized the entire stat structure.
        let status = unsafe { status.assume_init() };
        // SAFETY: fcntl inspects the fixed process descriptor, without borrowed memory.
        let flags = unsafe { libc::fcntl(libc::STDIN_FILENO, libc::F_GETFL) };
        if status.st_mode & libc::S_IFMT != libc::S_IFIFO
            || flags < 0
            || flags & libc::O_ACCMODE != libc::O_RDONLY
        {
            return Err(NativeFailure::InvalidRecord);
        }
        // SAFETY: the fixed stdin descriptor is exclusively managed by this entry
        // point. Setting CLOEXEC does not close it or change its read behaviour.
        if unsafe { libc::fcntl(libc::STDIN_FILENO, libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
            return Err(NativeFailure::IoUnavailable);
        }
        // SAFETY: F_DUPFD_CLOEXEC returns a fresh exclusively owned descriptor.
        let copy = unsafe { libc::fcntl(libc::STDIN_FILENO, libc::F_DUPFD_CLOEXEC, 3) };
        if copy < 0 {
            return Err(NativeFailure::IoUnavailable);
        }
        // SAFETY: copy is a new descriptor and has no other Rust owner.
        let pipe = unsafe { OwnedFd::from_raw_fd(copy) };
        // SAFETY: this process entry owns stdin. No retry on EINTR can accidentally
        // close a newly reused descriptor. The retained duplicate remains owned.
        if unsafe { libc::close(libc::STDIN_FILENO) } != 0 {
            return Err(NativeFailure::IoUnavailable);
        }
        Ok(Self { pipe })
    }

    /// Read one exact bounded request plus EOF under the original helper deadline.
    /// Fragmentation and EINTR do not restart the budget. No raw error is returned.
    pub fn read(self, deadline: HelperDeadline) -> Result<ManagementRequest, NativeFailure> {
        let mut bytes = [0u8; REQUEST_BYTES + 1];
        let mut length = 0;
        loop {
            let remaining = deadline.remaining()?;
            let milliseconds = remaining.as_millis().clamp(1, i32::MAX as u128) as i32;
            let mut poll = libc::pollfd {
                fd: self.pipe.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: poll points to one initialized pollfd for an owned live pipe.
            let ready = unsafe { libc::poll(&mut poll, 1, milliseconds) };
            if ready < 0 {
                if interrupted() {
                    continue;
                }
                return Err(NativeFailure::IoUnavailable);
            }
            if ready == 0 {
                continue;
            }
            deadline.check()?;
            // SAFETY: bytes has at least one spare oversize-probe byte; read writes
            // only its supplied initialized slice, and the pipe remains owned.
            let count = unsafe {
                libc::read(
                    self.pipe.as_raw_fd(),
                    bytes[length..].as_mut_ptr().cast(),
                    bytes.len() - length,
                )
            };
            if count < 0 {
                if interrupted() {
                    continue;
                }
                return Err(NativeFailure::IoUnavailable);
            }
            if count == 0 {
                return ManagementRequest::decode(&bytes[..length]);
            }
            length += count as usize;
            if length > REQUEST_BYTES {
                return Err(NativeFailure::InvalidRecord);
            }
        }
    }
}

fn interrupted() -> bool {
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR)
}
