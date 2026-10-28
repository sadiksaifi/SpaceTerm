//! Which local address a paired client's system would use to reach its server.
//!
//! A client's QUIC socket moves when a send fails or a ping goes unanswered. Neither happens when
//! a preferred network returns while the current one still works, as when Ethernet comes back
//! under Wi-Fi. [`PathWatch`] asks the routing table after each route-change signal and once per
//! ping interval, and says when the answer is an address the client's socket does not use.
use crate::protocol::limits;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::time::{Duration, Instant};

/// How often a client asks for its preferred local address without a route-change signal. Equal
/// to the protocol's ping interval, so an idle client wakes no more often than its keepalive.
pub(super) const CHECK_EVERY: Duration = limits::PING_AFTER;

/// The local address the system would use to reach a peer now.
pub(super) type Source = Box<dyn Fn(SocketAddr) -> Option<IpAddr> + Send>;

/// Asks the routing table. Connecting a UDP socket sends nothing.
pub(super) fn preferred_source(peer: SocketAddr) -> Option<IpAddr> {
    let local: SocketAddr = if peer.is_ipv4() {
        (Ipv4Addr::UNSPECIFIED, 0).into()
    } else {
        (Ipv6Addr::UNSPECIFIED, 0).into()
    };
    let probe = UdpSocket::bind(local).ok()?;
    probe.connect(peer).ok()?;
    probe.local_addr().ok().map(|address| address.ip())
}

/// Decides when a client moves to a socket on the local address the system now prefers.
pub(super) struct PathWatch {
    signal: Option<RouteSignal>,
    source: Source,
    /// The preferred address the client last moved for. One route change moves the client once,
    /// even when the new socket's address differs from the preferred one.
    moved_for: Option<IpAddr>,
    next: Instant,
}

impl PathWatch {
    pub(super) fn new(now: Instant) -> Self {
        Self {
            signal: RouteSignal::open(),
            source: Box::new(preferred_source),
            moved_for: None,
            next: now + CHECK_EVERY,
        }
    }

    /// Readable when the system's routes or addresses change.
    pub(super) fn descriptor(&self) -> Option<RawFd> {
        self.signal.as_ref().map(|signal| signal.0.as_raw_fd())
    }

    /// When the next check is due without a signal.
    pub(super) fn deadline(&self) -> Instant {
        self.next
    }

    /// After a route-change signal, or when the interval passed, asks for the preferred local
    /// address to `peer`. Calls `rebind` when it differs from `current`, the socket's address,
    /// unless the client already moved for it. `rebind` returns whether the client moved; a
    /// failed move is tried again at the next check. Returns whether the client moved.
    pub(super) fn follow(
        &mut self,
        now: Instant,
        peer: SocketAddr,
        current: Option<IpAddr>,
        rebind: impl FnOnce() -> bool,
    ) -> bool {
        let signalled = self.signal.as_ref().is_some_and(RouteSignal::changed);
        if !signalled && now < self.next {
            return false;
        }
        self.next = now + CHECK_EVERY;
        // Without a route the client stays; a failing send moves it once one returns.
        let Some(preferred) = (self.source)(peer) else {
            return false;
        };
        if current == Some(preferred) {
            self.moved_for = None;
            return false;
        }
        if self.moved_for == Some(preferred) || !rebind() {
            return false;
        }
        self.moved_for = Some(preferred);
        true
    }

    /// Replaces the routing-table lookup and makes the next check due at once.
    #[cfg(test)]
    pub(super) fn set_source(&mut self, source: Source) {
        self.source = source;
        self.next = Instant::now();
    }
}

/// A routing socket that turns readable when the system's routes or addresses change: a
/// `PF_ROUTE` socket on macOS, a netlink socket on Linux.
struct RouteSignal(OwnedFd);

impl RouteSignal {
    #[cfg(target_os = "linux")]
    fn open() -> Option<Self> {
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
    fn open() -> Option<Self> {
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
    fn open() -> Option<Self> {
        None
    }

    /// Reads every waiting message, so the descriptor is quiet until the next change. Returns
    /// whether any message reported a change.
    fn changed(&self) -> bool {
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
mod tests {
    use super::*;
    use std::os::unix::net::UnixDatagram;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    const PEER: &str = "192.0.2.1:9231";
    const WIRED: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 36));
    const WIRELESS: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 42));

    /// A watch with no route signal whose routing table answers `preferred` and counts lookups.
    fn watch(
        preferred: &Arc<Mutex<Option<IpAddr>>>,
        lookups: &Arc<AtomicUsize>,
        now: Instant,
    ) -> PathWatch {
        let (preferred, lookups) = (Arc::clone(preferred), Arc::clone(lookups));
        PathWatch {
            signal: None,
            source: Box::new(move |_| {
                lookups.fetch_add(1, Ordering::SeqCst);
                *preferred.lock().unwrap()
            }),
            moved_for: None,
            next: now,
        }
    }

    /// A message the platform's route signal counts as a change.
    fn route_added() -> Vec<u8> {
        let message = vec![0u8; 256];
        #[cfg(target_os = "macos")]
        let message = {
            let mut message = message;
            message[std::mem::offset_of!(libc::rt_msghdr, rtm_type)] = libc::RTM_ADD as u8;
            message
        };
        message
    }

    /// The Ethernet cable returns while Wi-Fi still works: the routing table prefers the wired
    /// address again, and the client on the wireless one moves once.
    #[test]
    fn a_client_moves_when_its_preferred_source_changes() {
        let start = Instant::now();
        let preferred = Arc::new(Mutex::new(Some(WIRELESS)));
        let lookups = Arc::new(AtomicUsize::new(0));
        let mut watch = watch(&preferred, &lookups, start);
        let peer = PEER.parse().unwrap();
        assert!(!watch.follow(start, peer, Some(WIRELESS), || unreachable!()));
        *preferred.lock().unwrap() = Some(WIRED);
        let mut rebinds = 0;
        assert!(watch.follow(start + CHECK_EVERY, peer, Some(WIRELESS), || {
            rebinds += 1;
            true
        }));
        assert_eq!(rebinds, 1);
    }

    /// While the routing table keeps preferring the socket's address, nothing moves.
    #[test]
    fn a_client_stays_while_its_preferred_source_is_its_own() {
        let start = Instant::now();
        let preferred = Arc::new(Mutex::new(Some(WIRED)));
        let lookups = Arc::new(AtomicUsize::new(0));
        let mut watch = watch(&preferred, &lookups, start);
        let peer = PEER.parse().unwrap();
        for interval in 0..5 {
            let now = start + CHECK_EVERY * interval;
            assert!(!watch.follow(now, peer, Some(WIRED), || unreachable!()));
        }
        assert_eq!(lookups.load(Ordering::SeqCst), 5);
        *preferred.lock().unwrap() = None;
        assert!(!watch.follow(
            start + CHECK_EVERY * 5,
            peer,
            Some(WIRED),
            || unreachable!()
        ));
    }

    /// A new socket whose address still differs from the preferred one never makes the client
    /// move again for the same route. A later change of route moves it again, and a failed move
    /// is retried at the next check.
    #[test]
    fn one_route_change_moves_a_client_at_most_once() {
        let start = Instant::now();
        let preferred = Arc::new(Mutex::new(Some(WIRED)));
        let lookups = Arc::new(AtomicUsize::new(0));
        let mut watch = watch(&preferred, &lookups, start);
        let peer = PEER.parse().unwrap();
        let mut rebinds = 0;
        for interval in 0..5 {
            watch.follow(start + CHECK_EVERY * interval, peer, Some(WIRELESS), || {
                rebinds += 1;
                true
            });
        }
        assert_eq!(rebinds, 1);

        let at = |interval| start + CHECK_EVERY * interval;
        *preferred.lock().unwrap() = Some(WIRELESS);
        assert!(!watch.follow(at(5), peer, Some(WIRELESS), || unreachable!()));
        *preferred.lock().unwrap() = Some(WIRED);
        assert!(!watch.follow(at(6), peer, Some(WIRELESS), || false));
        assert!(watch.follow(at(7), peer, Some(WIRELESS), || true));
    }

    /// Between intervals only a route-change signal makes the watch ask the routing table.
    #[test]
    fn a_client_checks_at_its_interval_or_on_a_route_signal() {
        let start = Instant::now();
        let preferred = Arc::new(Mutex::new(Some(WIRED)));
        let lookups = Arc::new(AtomicUsize::new(0));
        let mut watch = watch(&preferred, &lookups, start);
        let peer = PEER.parse().unwrap();
        watch.follow(start, peer, Some(WIRED), || true);
        assert_eq!(watch.deadline(), start + CHECK_EVERY);
        watch.follow(start + CHECK_EVERY / 2, peer, Some(WIRED), || true);
        assert_eq!(lookups.load(Ordering::SeqCst), 1);

        let (signal, system) = UnixDatagram::pair().unwrap();
        watch.signal = Some(RouteSignal(signal.into()));
        system.send(&route_added()).unwrap();
        watch.follow(start + CHECK_EVERY / 2, peer, Some(WIRED), || true);
        assert_eq!(lookups.load(Ordering::SeqCst), 2);
        assert_eq!(watch.deadline(), start + CHECK_EVERY / 2 + CHECK_EVERY);
    }

    /// The routing table answers with the address a connected socket would use, and macOS and
    /// Linux each give a client a route-change signal.
    #[test]
    fn the_system_names_a_preferred_source_and_signals_route_changes() {
        let loopback = SocketAddr::from((Ipv4Addr::LOCALHOST, 9));
        assert_eq!(preferred_source(loopback), Some(Ipv4Addr::LOCALHOST.into()));
        assert!(RouteSignal::open().is_some());
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
