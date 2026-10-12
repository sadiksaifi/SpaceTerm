//! Native Unix-domain socket peer facts. Same-account eligibility is not authentication.

use std::mem::size_of;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};

use tokio::net::UnixStream;

use crate::transport::{PeerFacts, TransportError, TransportRoute};

/// Checks the connected peer against the current effective UID using native credentials.
///
/// Linux uses credentials captured at connect/listen time. macOS uses `getpeereid`
/// and `LOCAL_PEERPID`. No path, environment value, or native error leaves this adapter.
pub fn same_account_peer(stream: &UnixStream) -> Result<PeerFacts, TransportError> {
    same_account_peer_fd(stream.as_fd())
}

pub(crate) fn same_account_peer_fd(stream: BorrowedFd<'_>) -> Result<PeerFacts, TransportError> {
    let (uid, pid) = native_peer(stream)?;
    // SAFETY: geteuid has no preconditions and does not dereference memory.
    if uid != unsafe { libc::geteuid() } {
        return Err(TransportError::PeerAccountRejected);
    }
    Ok(PeerFacts {
        route: TransportRoute::PrivateIpc,
        local_uid: Some(uid),
        local_pid: Some(pid),
        pinned_server_key: None,
    })
}

#[cfg(target_os = "linux")]
fn native_peer(stream: BorrowedFd<'_>) -> Result<(u32, u32), TransportError> {
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length = size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: the output and length are writable, correctly sized, and the socket stays live.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut credentials).cast(),
            &raw mut length,
        )
    };
    if result != 0 || length as usize != size_of::<libc::ucred>() || credentials.pid < 1 {
        return Err(TransportError::PeerAccountRejected);
    }
    Ok((credentials.uid, credentials.pid as u32))
}

#[cfg(target_os = "macos")]
fn native_peer(stream: BorrowedFd<'_>) -> Result<(u32, u32), TransportError> {
    let mut uid = 0;
    let mut gid = 0;
    // SAFETY: getpeereid writes only the supplied uid and gid for this live socket.
    if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } != 0 {
        return Err(TransportError::PeerAccountRejected);
    }
    let mut pid: libc::pid_t = 0;
    let mut length = size_of::<libc::pid_t>() as libc::socklen_t;
    // SAFETY: the output and length are writable, correctly sized, and the socket stays live.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            (&raw mut pid).cast(),
            &raw mut length,
        )
    };
    if result != 0 || length as usize != size_of::<libc::pid_t>() || pid < 1 {
        return Err(TransportError::PeerAccountRejected);
    }
    Ok((uid, pid as u32))
}
