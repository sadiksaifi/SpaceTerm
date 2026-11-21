//! Which local address a paired client's system would use to reach its server.
//!
//! A client's QUIC socket moves when a send fails or a ping goes unanswered. Neither happens when
//! a preferred network returns while the current one still works, as when Ethernet comes back
//! under Wi-Fi. The carrier asks the routing table after each route-change signal and once per
//! ping interval, and says when the answer is an address the client's socket does not use.
use crate::protocol::limits;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::os::fd::RawFd;
use std::time::{Duration, Instant};

/// A per-connection signal that becomes readable when the system's routes change.
/// `changed` drains pending notifications without blocking.
pub trait PathSignal: Send {
    fn descriptor(&self) -> RawFd;
    fn changed(&self) -> bool;
}

/// Opens an independent signal for each work connection. Returning `None` leaves
/// the periodic route check and the QUIC recovery triggers active.
pub type NetworkPath = std::sync::Arc<dyn Fn() -> Option<Box<dyn PathSignal>> + Send + Sync>;

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
    signal: Option<Box<dyn PathSignal>>,
    source: Source,
    /// The preferred address the client last moved for. One route change moves the client once,
    /// even when the new socket's address differs from the preferred one.
    moved_for: Option<IpAddr>,
    next: Instant,
}

impl PathWatch {
    pub(super) fn new(now: Instant, signal: Option<Box<dyn PathSignal>>) -> Self {
        Self {
            signal,
            source: Box::new(preferred_source),
            moved_for: None,
            next: now + CHECK_EVERY,
        }
    }

    /// Readable when the system's routes or addresses change.
    pub(super) fn descriptor(&self) -> Option<RawFd> {
        self.signal.as_ref().map(|signal| signal.descriptor())
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
        let signalled = self.signal.as_ref().is_some_and(|signal| signal.changed());
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
    #[cfg(any(test, feature = "core-transport"))]
    pub(super) fn set_source(&mut self, source: Source) {
        self.source = source;
        self.next = Instant::now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

        use std::os::fd::AsRawFd;
        use std::os::unix::net::UnixDatagram;

        struct Signal(UnixDatagram);
        impl PathSignal for Signal {
            fn descriptor(&self) -> RawFd {
                self.0.as_raw_fd()
            }
            fn changed(&self) -> bool {
                self.0.recv(&mut [0]).is_ok()
            }
        }

        let (signal, system) = UnixDatagram::pair().unwrap();
        signal.set_nonblocking(true).unwrap();
        watch.signal = Some(Box::new(Signal(signal)));
        system.send(&[1]).unwrap();
        watch.follow(start + CHECK_EVERY / 2, peer, Some(WIRED), || true);
        assert_eq!(lookups.load(Ordering::SeqCst), 2);
        assert_eq!(watch.deadline(), start + CHECK_EVERY / 2 + CHECK_EVERY);
    }

    /// The routing table answers with the address a connected socket would use, and macOS and
    /// Linux each give a client a route-change signal.
    #[test]
    fn the_system_names_a_preferred_source() {
        let loopback = SocketAddr::from((Ipv4Addr::LOCALHOST, 9));
        assert_eq!(preferred_source(loopback), Some(Ipv4Addr::LOCALHOST.into()));
    }
}
