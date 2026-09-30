//! Linux local-socket peer credentials for AskPass authentication.
//!
//! `SO_PEERCRED` records the peer credentials at `connect` or `listen` time, so the helper side
//! observes the broker process exactly as macOS `LOCAL_PEERPID` does.

use std::io;
use std::mem::size_of;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;

/// The effective user of the connected peer.
pub(super) fn peer_user(stream: &UnixStream) -> io::Result<libc::uid_t> {
    peer_credentials(stream).map(|credentials| credentials.uid)
}

/// The process that connected, or listened on, the peer socket endpoint.
pub(super) fn peer_process(stream: &UnixStream) -> io::Result<libc::pid_t> {
    peer_credentials(stream).map(|credentials| credentials.pid)
}

fn peer_credentials(stream: &UnixStream) -> io::Result<libc::ucred> {
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length = libc::socklen_t::try_from(size_of::<libc::ucred>())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    // SAFETY: the output pointer and length describe a writable `ucred`; the socket remains live
    // for the call, and `getsockopt` does not retain either pointer.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut credentials).cast::<libc::c_void>(),
            &raw mut length,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    if length as usize != size_of::<libc::ucred>() || credentials.pid < 1 {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    Ok(credentials)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_peer_credentials_identify_the_connected_process_and_user() {
        let (first, _second) = UnixStream::pair().unwrap();
        // SAFETY: getpid and geteuid have no preconditions.
        let (process, user) = unsafe { (libc::getpid(), libc::geteuid()) };

        assert_eq!(peer_process(&first).unwrap(), process);
        assert_eq!(peer_user(&first).unwrap(), user);
    }
}
