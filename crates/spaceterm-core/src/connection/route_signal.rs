//! A signal that the system's routes or addresses changed.
//!
//! A paired client asks for its preferred local address after each one, and SpaceTerm Core binds
//! a Network Access address it waits for again.
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

/// A routing socket that turns readable when the system's routes or addresses change: a
/// `PF_ROUTE` socket on macOS, a netlink socket on Linux.
pub(crate) struct RouteSignal(pub(super) OwnedFd);

impl RouteSignal {
    /// Readable while a change waits to be read.
    pub(crate) fn descriptor(&self) -> RawFd {
        self.0.as_raw_fd()
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn open() -> Option<Self> {
        // SAFETY: socket takes plain integer arguments and returns a new descriptor or -1.
        let fd = unsafe {
            libc::socket(
                libc::AF_NETLINK,
                libc::SOCK_RAW | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
                libc::NETLINK_ROUTE,
            )
        };
        if fd < 0 {
            return None;
        }
        // SAFETY: fd is a new descriptor this function owns.
        let signal = Self(unsafe { OwnedFd::from_raw_fd(fd) });
        // SAFETY: an all-zero sockaddr_nl is valid; its fields are set below.
        let mut address: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
        address.nl_family = libc::AF_NETLINK as libc::sa_family_t;
        // These groups carry only address and route changes, so every message counts.
        address.nl_groups = (libc::RTMGRP_IPV4_IFADDR
            | libc::RTMGRP_IPV6_IFADDR
            | libc::RTMGRP_IPV4_ROUTE
            | libc::RTMGRP_IPV6_ROUTE) as u32;
        // SAFETY: bind reads one sockaddr_nl of the given size from a live local.
        let bound = unsafe {
            libc::bind(
                fd,
                (&raw const address).cast(),
                std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
            )
        };
        (bound == 0).then_some(signal)
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn open() -> Option<Self> {
        // SAFETY: socket takes plain integer arguments and returns a new descriptor or -1.
        let fd = unsafe { libc::socket(libc::PF_ROUTE, libc::SOCK_RAW, libc::AF_UNSPEC) };
        if fd < 0 {
            return None;
        }
        // SAFETY: fd is a new descriptor this function owns.
        let signal = Self(unsafe { OwnedFd::from_raw_fd(fd) });
        // SAFETY: fcntl sets the descriptor flag of the live descriptor above.
        (unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } == 0).then_some(signal)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    pub(crate) fn open() -> Option<Self> {
        None
    }

    /// Reads every waiting message, so the descriptor is quiet until the next change. Returns
    /// whether any message reported a change.
    pub(crate) fn changed(&self) -> bool {
        let mut buffer = [0u8; 4096];
        let mut changed = false;
        let mut overflowed = false;
        loop {
            // SAFETY: recv writes at most `buffer.len()` bytes into a live local buffer from
            // this live descriptor, without blocking.
            let count = unsafe {
                libc::recv(
                    self.0.as_raw_fd(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                    libc::MSG_DONTWAIT,
                )
            };
            if count > 0 {
                changed |= reports_change(&buffer[..count as usize]);
                continue;
            }
            match std::io::Error::last_os_error().raw_os_error() {
                // The kernel dropped messages for this socket, so some change went unread. It
                // reports that once; the messages after it are still read.
                Some(libc::ENOBUFS) if count < 0 && !overflowed => {
                    overflowed = true;
                    changed = true;
                }
                Some(libc::EINTR) if count < 0 => {}
                _ => return changed,
            }
        }
    }
}

/// Every message on the subscribed netlink groups reports an address or route change.
#[cfg(not(target_os = "macos"))]
fn reports_change(_: &[u8]) -> bool {
    true
}

/// Route additions, deletions and changes, and address changes. Misses, lookups, neighbor
/// entries and routes cloned for one destination change no preferred source.
#[cfg(target_os = "macos")]
fn reports_change(message: &[u8]) -> bool {
    let kind = std::mem::offset_of!(libc::rt_msghdr, rtm_type);
    let flags = std::mem::offset_of!(libc::rt_msghdr, rtm_flags);
    let Some(&kind) = message.get(kind) else {
        return false;
    };
    match i32::from(kind) {
        libc::RTM_NEWADDR | libc::RTM_DELADDR => true,
        libc::RTM_ADD | libc::RTM_DELETE | libc::RTM_CHANGE => message
            .get(flags..flags + 4)
            .and_then(|bytes| bytes.try_into().ok())
            .map(i32::from_ne_bytes)
            .is_some_and(|flags| flags & (libc::RTF_LLINFO | libc::RTF_WASCLONED) == 0),
        _ => false,
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use std::os::unix::net::UnixDatagram;

    /// A message the platform's route signal counts as a change.
    pub(in crate::connection) fn route_added() -> Vec<u8> {
        let message = vec![0u8; 256];
        #[cfg(target_os = "macos")]
        let message = {
            let mut message = message;
            message[std::mem::offset_of!(libc::rt_msghdr, rtm_type)] = libc::RTM_ADD as u8;
            message
        };
        message
    }

    /// A signal reads every waiting message, so the descriptor stops being readable and the I/O
    /// thread's next wait blocks until the next change.
    #[test]
    fn a_route_signal_drains_every_message_so_its_descriptor_goes_quiet() {
        let (signal, system) = UnixDatagram::pair().unwrap();
        let signal = RouteSignal(signal.into());
        for _ in 0..3 {
            system.send(&route_added()).unwrap();
        }
        let readable = || {
            let mut descriptor = libc::pollfd {
                fd: signal.0.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: `descriptor` is a live, writable `pollfd` for the whole call.
            unsafe { libc::poll(&mut descriptor, 1, 0) == 1 }
        };
        assert!(readable());
        assert!(signal.changed());
        assert!(!readable());
        assert!(!signal.changed());
    }

    /// On macOS a routing socket also reports misses, neighbor entries and cloned host routes,
    /// which arrive every few seconds on a busy network and change no preferred source.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_macos_route_signal_ignores_misses_and_neighbor_entries() {
        let message = |kind: i32, flags: i32| {
            let mut message = vec![0u8; std::mem::size_of::<libc::rt_msghdr>()];
            message[std::mem::offset_of!(libc::rt_msghdr, rtm_type)] = kind as u8;
            let at = std::mem::offset_of!(libc::rt_msghdr, rtm_flags);
            message[at..at + 4].copy_from_slice(&flags.to_ne_bytes());
            message
        };
        assert!(reports_change(&message(libc::RTM_ADD, libc::RTF_GATEWAY)));
        assert!(reports_change(&message(
            libc::RTM_DELETE,
            libc::RTF_GATEWAY
        )));
        assert!(reports_change(&message(libc::RTM_NEWADDR, 0)));
        assert!(!reports_change(&message(libc::RTM_MISS, 0)));
        assert!(!reports_change(&message(libc::RTM_ADD, libc::RTF_LLINFO)));
        assert!(!reports_change(&message(
            libc::RTM_ADD,
            libc::RTF_WASCLONED
        )));
        assert!(!reports_change(&[]));
    }
}
