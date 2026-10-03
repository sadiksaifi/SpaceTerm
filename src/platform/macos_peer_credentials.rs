//! macOS local-socket peer credentials for AskPass authentication.

use std::io;
use std::mem::size_of;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;

/// The effective user of the connected peer.
pub(super) fn peer_user(stream: &UnixStream) -> io::Result<libc::uid_t> {
    let mut peer_user = 0;
    let mut peer_group = 0;
    // SAFETY: `getpeereid` only writes the supplied uid and gid values for this live socket.
    let result = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut peer_user, &mut peer_group) };
    if result == 0 {
        Ok(peer_user)
    } else {
        Err(io::Error::last_os_error())
    }
}

/// The process that connected, or listened on, the peer socket endpoint.
pub(super) fn peer_process(stream: &UnixStream) -> io::Result<libc::pid_t> {
    let mut peer_process: libc::pid_t = 0;
    let mut peer_process_size = libc::socklen_t::try_from(size_of::<libc::pid_t>())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    // SAFETY: the output pointer and length describe a writable `pid_t`; the socket remains live
    // for the call, and `getsockopt` does not retain either pointer.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            (&raw mut peer_process).cast::<libc::c_void>(),
            &raw mut peer_process_size,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    if peer_process_size as usize != size_of::<libc::pid_t>() {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    Ok(peer_process)
}
