//! Private stdin preserved by sudo and pkexec. Native caller proof requires the anonymous stream route.

pub use super::facade::socket_caller::BrokerCallerPeer;
use super::facade::socket_caller::Sender;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;

use crate::native_deployment::contracts::NativeFailure;

use super::helper_lifetime::HelperDeadline;
use super::request::{ManagementRequest, REQUEST_BYTES};

/// Own the manager's fixed stdin. An ordinary pipe supplies no caller authority.
pub struct RequestStdin {
    pipe: OwnedFd,
    socket: bool,
}
impl RequestStdin {
    /// Claim stdin at one-request manager entry, before any subsequent child.
    ///
    /// The composition root must not have created another stdin reader. Only a
    /// read-only pipe or connected local stream is accepted. Both the original and retained descriptor are
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
        let kind = status.st_mode & libc::S_IFMT;
        if flags < 0
            || !matches!(kind, libc::S_IFIFO | libc::S_IFSOCK)
            || (kind == libc::S_IFIFO && flags & libc::O_ACCMODE != libc::O_RDONLY)
        {
            return Err(NativeFailure::InvalidRecord);
        }
        if kind == libc::S_IFSOCK {
            validate_stream(libc::STDIN_FILENO)?;
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
        Ok(Self {
            pipe,
            socket: kind == libc::S_IFSOCK,
        })
    }

    /// Whether this transport can carry kernel-authenticated sender evidence.
    /// This observation is not caller proof or authorization.
    pub fn is_socket(&self) -> bool {
        self.socket
    }

    /// Receives the exact request with a kernel-authenticated sender retained through admission.
    /// An ordinary pipe cannot provide caller identity.
    pub fn read_authenticated(
        self,
        deadline: HelperDeadline,
    ) -> Result<(ManagementRequest, BrokerCallerPeer), NativeFailure> {
        validate_stream(self.pipe.as_raw_fd())?;
        let mut bytes = [0; REQUEST_BYTES];
        let mut used = 0;
        let mut sender = None;
        while used < bytes.len() {
            wait(self.pipe.as_raw_fd(), libc::POLLIN, deadline)?;
            let (count, credentials) = receive(&self.pipe, &mut bytes[used..])?;
            if count == 0 {
                return Err(NativeFailure::InvalidRecord);
            }
            if let Some(original) = &sender {
                let original: &Sender = original;
                if !original.matches(&credentials) {
                    return Err(NativeFailure::IdentityMismatch);
                }
                original.recheck()?;
            } else {
                sender = Some(Sender::capture(credentials, &self.pipe)?);
            }
            used += count;
        }
        let request = ManagementRequest::decode(&bytes)?;
        let sender = sender.ok_or(NativeFailure::AuthorizationUnavailable)?;
        if sender.uid() != request.key().account_uid {
            return Err(NativeFailure::IdentityMismatch);
        }
        let mut challenge = zeroize::Zeroizing::new([0u8; 32]);
        getrandom::fill(challenge.as_mut()).map_err(|_| NativeFailure::NativeUnavailable)?;
        sender.recheck()?;
        send(&self.pipe, &challenge[..], deadline)?;
        let mut echoed = zeroize::Zeroizing::new([0u8; 33]);
        let mut used = 0;
        loop {
            wait(self.pipe.as_raw_fd(), libc::POLLIN, deadline)?;
            let (count, credentials) = receive(&self.pipe, &mut echoed[used..])?;
            if count == 0 {
                break;
            }
            if !sender.matches(&credentials) {
                return Err(NativeFailure::IdentityMismatch);
            }
            sender.recheck()?;
            used += count;
            if used > 32 {
                return Err(NativeFailure::InvalidRecord);
            }
        }
        if used != 32 || echoed[..32] != challenge[..] {
            return Err(NativeFailure::AuthorizationDenied);
        }
        // The second kernel-authenticated write was made while this process incarnation was pinned.
        // A queued first message from a dead PID cannot pass this challenge.
        sender.recheck()?;
        deadline.check()?;
        Ok((request, BrokerCallerPeer::new(sender)))
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

/// Creates only an anonymous local management channel. No named endpoint or caller path is used.
/// Kernel credential delivery is enabled before any broker or sender can write.
pub fn broker_request_channel() -> Result<(UnixStream, OwnedFd), NativeFailure> {
    let (writer, reader) = UnixStream::pair().map_err(|_| NativeFailure::IoUnavailable)?;
    #[cfg(target_os = "linux")]
    {
        let enabled: libc::c_int = 1;
        // SAFETY: setsockopt copies the exact initialized scalar into the owned local receiver.
        if unsafe {
            libc::setsockopt(
                reader.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PASSCRED,
                std::ptr::from_ref(&enabled).cast(),
                std::mem::size_of_val(&enabled) as libc::socklen_t,
            )
        } != 0
        {
            return Err(NativeFailure::IoUnavailable);
        }
    }
    #[cfg(target_os = "macos")]
    for stream in [&writer, &reader] {
        let enabled: libc::c_int = 1;
        // SAFETY: SO_NOSIGPIPE affects only this owned descriptor, never another thread's signal mask.
        if unsafe {
            libc::setsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_NOSIGPIPE,
                std::ptr::from_ref(&enabled).cast(),
                std::mem::size_of_val(&enabled) as libc::socklen_t,
            )
        } != 0
        {
            return Err(NativeFailure::IoUnavailable);
        }
    }
    Ok((writer, reader.into()))
}
fn validate_stream(fd: i32) -> Result<(), NativeFailure> {
    let mut kind: libc::c_int = 0;
    let mut length = std::mem::size_of_val(&kind) as libc::socklen_t;
    // SAFETY: getsockopt initializes the supplied scalar and length with no pointer escape.
    if unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_TYPE,
            std::ptr::from_mut(&mut kind).cast(),
            &mut length,
        )
    } != 0
        || kind != libc::SOCK_STREAM
    {
        return Err(NativeFailure::AuthorizationUnavailable);
    }
    let mut address = std::mem::MaybeUninit::<libc::sockaddr_storage>::zeroed();
    let mut length = std::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;
    // SAFETY: getpeername initializes at most this exact address buffer.
    if unsafe { libc::getpeername(fd, address.as_mut_ptr().cast(), &mut length) } != 0 {
        return Err(NativeFailure::AuthorizationUnavailable);
    }
    // SAFETY: success initialized the address family.
    if unsafe { address.assume_init() }.ss_family as i32 != libc::AF_UNIX {
        return Err(NativeFailure::AuthorizationUnavailable);
    }
    Ok(())
}
fn wait(fd: i32, events: i16, deadline: HelperDeadline) -> Result<(), NativeFailure> {
    loop {
        let mut status = libc::pollfd {
            fd,
            events,
            revents: 0,
        };
        let remaining = deadline.remaining()?.as_millis().clamp(1, i32::MAX as u128) as i32;
        // SAFETY: poll receives one initialized descriptor and retains no pointer.
        let result = unsafe { libc::poll(&mut status, 1, remaining) };
        if result < 0 && interrupted() {
            continue;
        }
        if result < 0 || status.revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
            return Err(NativeFailure::IoUnavailable);
        }
        deadline.check()?;
        if result != 0 {
            return Ok(());
        }
    }
}
fn send(channel: &OwnedFd, bytes: &[u8], deadline: HelperDeadline) -> Result<(), NativeFailure> {
    let mut used = 0;
    while used < bytes.len() {
        wait(channel.as_raw_fd(), libc::POLLOUT, deadline)?;
        #[cfg(target_os = "linux")]
        let flags = libc::MSG_NOSIGNAL;
        #[cfg(target_os = "macos")]
        let flags = 0; // The channel's descriptor-local SO_NOSIGPIPE is set before broker spawn.
        // SAFETY: send borrows the bounded initialized suffix and retains no pointer.
        let count = unsafe {
            libc::send(
                channel.as_raw_fd(),
                bytes[used..].as_ptr().cast(),
                bytes.len() - used,
                flags,
            )
        };
        if count < 0 && interrupted() {
            continue;
        }
        if count <= 0 {
            return Err(NativeFailure::IoUnavailable);
        }
        used += count as usize;
    }
    Ok(())
}
#[cfg(target_os = "macos")]
fn receive(channel: &OwnedFd, bytes: &mut [u8]) -> Result<(usize, ()), NativeFailure> {
    // SAFETY: recv writes only the bounded caller-owned buffer.
    let count = unsafe {
        libc::recv(
            channel.as_raw_fd(),
            bytes.as_mut_ptr().cast(),
            bytes.len(),
            0,
        )
    };
    if count < 0 {
        return Err(NativeFailure::IoUnavailable);
    }
    Ok((count as usize, ()))
}
#[cfg(target_os = "linux")]
fn receive(channel: &OwnedFd, bytes: &mut [u8]) -> Result<(usize, libc::ucred), NativeFailure> {
    // usize alignment accommodates native cmsghdr. Received rights are always closed, never retained.
    let mut control = [0usize; 32];
    let mut vector = libc::iovec {
        iov_base: bytes.as_mut_ptr().cast(),
        iov_len: bytes.len(),
    };
    // SAFETY: all-zero msghdr is valid with no name or ancillary pointers before we assign the owned buffers.
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut vector;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = std::mem::size_of_val(&control);
    // SAFETY: recvmsg fills only these initialized, bounded buffers; rights arrive CLOEXEC.
    let count = unsafe { libc::recvmsg(channel.as_raw_fd(), &mut message, libc::MSG_CMSG_CLOEXEC) };
    if count < 0 {
        return Err(NativeFailure::IoUnavailable);
    }
    let mut credential = None;
    let mut unexpected = message.msg_flags & (libc::MSG_CTRUNC | libc::MSG_TRUNC) != 0;
    // SAFETY: native CMSG traversal stays within the kernel-filled msg_control extent.
    let mut header = unsafe { libc::CMSG_FIRSTHDR(&message) };
    while !header.is_null() {
        // SAFETY: kernel-generated ancillary headers and their payloads are initialized and bounded by recvmsg.
        unsafe {
            let bytes = (*header)
                .cmsg_len
                .saturating_sub(libc::CMSG_LEN(0) as usize);
            if (*header).cmsg_level == libc::SOL_SOCKET
                && (*header).cmsg_type == libc::SCM_CREDENTIALS
                && bytes == std::mem::size_of::<libc::ucred>()
                && credential.is_none()
            {
                credential = Some(std::ptr::read_unaligned(
                    libc::CMSG_DATA(header).cast::<libc::ucred>(),
                ));
            } else {
                unexpected = true;
                if (*header).cmsg_level == libc::SOL_SOCKET
                    && (*header).cmsg_type == libc::SCM_RIGHTS
                {
                    for offset in 0..bytes / std::mem::size_of::<i32>() {
                        libc::close(std::ptr::read_unaligned(
                            libc::CMSG_DATA(header).cast::<i32>().add(offset),
                        ));
                    }
                }
            }
            header = libc::CMSG_NXTHDR(&message, header);
        }
    }
    if unexpected {
        return Err(NativeFailure::InvalidRecord);
    }
    if count == 0 {
        return Ok((
            0,
            libc::ucred {
                pid: 0,
                uid: 0,
                gid: 0,
            },
        ));
    }
    Ok((
        count as usize,
        credential.ok_or(NativeFailure::AuthorizationUnavailable)?,
    ))
}

fn interrupted() -> bool {
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR)
}
