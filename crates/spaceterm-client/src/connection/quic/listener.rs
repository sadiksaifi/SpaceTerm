//! Routes datagrams on the Core's listening UDP sockets and retains draining connections.
use super::*;
use noq_proto::{ConnectionEvent, EndpointEvent};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::AtomicU8;
use std::sync::{Mutex, MutexGuard, RwLock, RwLockReadGuard};

/// A listener's sockets: one at its address, or one per family for every interface. Each reply
/// leaves from the address its client dialed, which packet info reports on receipt.
pub(super) struct Sockets {
    pub(super) address: SocketAddr,
    udp: Vec<Udp>,
    /// An old address a failed widening could neither replace nor bind again. The listener's
    /// thread binds it again at each turn until it can.
    lost: Option<SocketAddr>,
}

impl Sockets {
    /// Binds `address`. The unspecified address of either family binds `0.0.0.0` and `[::]` at
    /// the same port, the IPv6 one only where the system offers IPv6.
    pub(super) fn bind(address: SocketAddr) -> Result<Self, QuicError> {
        if !address.ip().is_unspecified() {
            let udp = Udp::bind(address)?;
            let port = udp.port().ok_or(QuicError::Transport)?;
            return Ok(Self {
                address: SocketAddr::new(address.ip(), port),
                udp: vec![udp],
                lost: None,
            });
        }
        let v4 = Udp::bind((std::net::Ipv4Addr::UNSPECIFIED, address.port()).into())?;
        let port = v4.port().ok_or(QuicError::Transport)?;
        let mut udp = vec![v4];
        match Udp::bind((std::net::Ipv6Addr::UNSPECIFIED, port).into()) {
            Ok(v6) => udp.push(v6),
            Err(QuicError::AddressInUse) => return Err(QuicError::AddressInUse),
            // A system without IPv6 refuses the socket or the address.
            Err(_) => {}
        }
        Ok(Self {
            address: (std::net::Ipv4Addr::UNSPECIFIED, port).into(),
            udp,
            lost: None,
        })
    }

    /// Widens one address to every interface at its port. The family the old socket lacks binds
    /// first, so a port taken there leaves the old socket in place. An address and the wildcard
    /// of its port can't both be bound, so the old socket closes just before its family's
    /// wildcard binds; a datagram arriving in between is lost and QUIC sends it again. When that
    /// bind fails the old address binds again, and the listener keeps a socket either way.
    pub(super) fn widen(&mut self) -> Result<(), QuicError> {
        let port = self.address.port();
        let wildcard = |ipv4: bool| -> SocketAddr {
            if ipv4 {
                (std::net::Ipv4Addr::UNSPECIFIED, port).into()
            } else {
                (std::net::Ipv6Addr::UNSPECIFIED, port).into()
            }
        };
        let [old] = std::mem::take(&mut self.udp)
            .try_into()
            .map_err(|udp: Vec<Udp>| {
                self.udp = udp;
                QuicError::Configuration
            })?;
        let other = match Udp::bind(wildcard(!old.ipv4)) {
            Ok(other) => Some(other),
            Err(QuicError::AddressInUse) => {
                self.udp = vec![old];
                return Err(QuicError::AddressInUse);
            }
            // A system without IPv6 refuses the socket or the address.
            Err(_) if old.ipv4 => None,
            Err(error) => {
                self.udp = vec![old];
                return Err(error);
            }
        };
        let (previous, ipv4) = (self.address, old.ipv4);
        drop(old);
        match Udp::bind(wildcard(ipv4)) {
            Ok(widened) => {
                self.udp = std::iter::once(widened).chain(other).collect();
                self.address = wildcard(true);
                Ok(())
            }
            Err(error) => {
                match Udp::bind(previous) {
                    Ok(restored) => self.udp = vec![restored],
                    Err(_) => {
                        self.udp = other.into_iter().collect();
                        self.lost = Some(previous);
                    }
                }
                Err(error)
            }
        }
    }

    /// Binds a lost old address again. Returns whether one is still lost.
    fn recover(&mut self) -> bool {
        let Some(lost) = self.lost else {
            return false;
        };
        if let Ok(restored) = Udp::bind(lost) {
            self.udp.retain(|udp| udp.ipv4 != restored.ipv4);
            self.udp.push(restored);
            self.lost = None;
        }
        self.lost.is_some()
    }

    pub(super) fn for_destination(&self, destination: SocketAddr) -> Option<&Udp> {
        self.udp
            .iter()
            .find(|udp| udp.ipv4 == destination.is_ipv4())
    }
}

fn bell() -> Result<(UnixStream, UnixStream), QuicError> {
    let (bell, ring) = UnixStream::pair().map_err(|_| QuicError::Transport)?;
    bell.set_nonblocking(true)
        .and_then(|()| ring.set_nonblocking(true))
        .map_err(|_| QuicError::Transport)?;
    Ok((bell, ring))
}

pub(super) fn ring(ring: &UnixStream) {
    // A full socket already wakes the thread, so a refused byte is harmless.
    let _ = (&*ring).write(&[1]);
}

pub(super) fn drain_bell(bell: &UnixStream) {
    let mut bytes = [0; 64];
    while matches!((&*bell).read(&mut bytes), Ok(count) if count > 0) {}
}

/// The server's endpoint and the connections it routes datagrams to.
pub(super) struct Server {
    pub(super) endpoint: Endpoint,
    pub(super) routes: HashMap<ConnectionHandle, Arc<Route>>,
    /// Closed connections whose threads ended. The listener's thread answers their peers with
    /// the close again until they drain, so one lost close never reads as a reset.
    draining: HashMap<ConnectionHandle, Draining>,
}

/// A closed connection the listener's thread drives until it drains.
struct Draining {
    connection: Connection,
    until: Instant,
    sends: usize,
}

impl Server {
    /// Takes over a closed connection whose thread ends, or forgets it when it already drained
    /// or too many drain.
    pub(super) fn drain(&mut self, handle: ConnectionHandle, connection: Connection, now: Instant) {
        // A drained connection already gave up its handle and route, and the endpoint may have
        // given the handle to a new connection whose route this must keep.
        if connection.is_drained() {
            return;
        }
        self.routes.remove(&handle);
        if self.draining.len() >= MAX_DRAINING {
            self.endpoint.handle_event(handle, EndpointEvent::drained());
            return;
        }
        // QUIC's own close timer is three probe timeouts away.
        let until = connection
            .poll_timeout()
            .unwrap_or(now)
            .clamp(now + DRAIN_MIN, now + DRAIN_MAX);
        self.draining.insert(
            handle,
            Draining {
                connection,
                until,
                sends: 0,
            },
        );
    }

    /// When the next draining connection ends.
    fn next_drained(&self) -> Option<Instant> {
        self.draining.values().map(|draining| draining.until).min()
    }

    /// Forgets the draining connections whose time ended.
    pub(super) fn expire(&mut self, now: Instant) {
        let ended: Vec<_> = self
            .draining
            .iter()
            .filter(|(_, draining)| draining.until <= now)
            .map(|(handle, _)| *handle)
            .collect();
        for handle in ended {
            self.forget(handle);
        }
    }

    pub(super) fn forget(&mut self, handle: ConnectionHandle) {
        if self.draining.remove(&handle).is_some() {
            self.endpoint.handle_event(handle, EndpointEvent::drained());
        }
    }

    /// Hands a datagram to a draining connection and sends what it answers: its close again.
    /// QUIC's own close timer never runs here, so the connection stays closed until `until`.
    fn answer_draining(
        &mut self,
        handle: ConnectionHandle,
        event: ConnectionEvent,
        now: Instant,
        shared: &Shared,
        buffer: &mut Vec<u8>,
    ) {
        let Some(draining) = self.draining.get_mut(&handle) else {
            return;
        };
        draining.connection.handle_event(event);
        while draining.sends < DRAIN_SENDS {
            buffer.clear();
            let Some(transmit) =
                draining
                    .connection
                    .poll_transmit(now, std::num::NonZeroUsize::MIN, buffer)
            else {
                break;
            };
            draining.sends += 1;
            let _ = shared.send(
                &buffer[..transmit.size],
                transmit.destination,
                transmit.src_ip,
            );
        }
        let mut drained = false;
        while let Some(event) = draining.connection.poll_endpoint_events() {
            drained |= event.is_drained();
            if !drained && let Some(event) = self.endpoint.handle_event(handle, event) {
                draining.connection.handle_event(event);
            }
        }
        // A stateless reset or the peer's own close can drain the connection early.
        if drained || draining.connection.is_drained() {
            self.forget(handle);
        }
    }
}

/// Datagrams for one server connection, waiting for its thread.
pub(super) struct Route {
    pub(super) events: Mutex<Bounded<ConnectionEvent>>,
    pub(super) ring: UnixStream,
}

/// A queue that refuses items past a count and a byte total.
pub(super) struct Bounded<T> {
    items: VecDeque<T>,
    bytes: usize,
}

impl<T> Default for Bounded<T> {
    fn default() -> Self {
        Self {
            items: VecDeque::new(),
            bytes: 0,
        }
    }
}

impl<T> Bounded<T> {
    /// Queues `item` of `bytes` unless that passes [`ROUTED_DATAGRAMS`] or [`ROUTED_BYTES`].
    pub(super) fn push(&mut self, item: T, bytes: usize) -> bool {
        if self.items.len() >= ROUTED_DATAGRAMS || self.bytes + bytes > ROUTED_BYTES {
            return false;
        }
        self.items.push_back(item);
        self.bytes += bytes;
        true
    }

    pub(super) fn take(&mut self) -> VecDeque<T> {
        self.bytes = 0;
        std::mem::take(&mut self.items)
    }
}

pub(super) struct Shared {
    server: Mutex<Server>,
    /// Replaced only when the listener widens to every interface.
    sockets: RwLock<Sockets>,
    /// Wakes the listener's thread: to stop, to widen, or to drain a connection whose thread
    /// ended.
    pub(super) ring: UnixStream,
}

impl Shared {
    pub(super) fn lock(&self) -> MutexGuard<'_, Server> {
        self.server.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Hands a closed connection whose thread ends to the listener's thread.
    pub(super) fn drain(&self, handle: ConnectionHandle, connection: Connection, now: Instant) {
        self.lock().drain(handle, connection, now);
        ring(&self.ring);
    }

    pub(super) fn sockets(&self) -> RwLockReadGuard<'_, Sockets> {
        self.sockets.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Sends one datagram from the socket of `destination`'s family.
    pub(super) fn send(
        &self,
        bytes: &[u8],
        destination: SocketAddr,
        source: Option<std::net::IpAddr>,
    ) -> std::io::Result<()> {
        match self.sockets().for_destination(destination) {
            Some(udp) => udp.send(bytes, destination, source),
            None => Err(ErrorKind::AddrNotAvailable.into()),
        }
    }

    pub(super) fn widen(&self) -> Result<(), QuicError> {
        self.sockets
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .widen()
    }

    /// Binds a lost old address again. Returns whether one is still lost.
    fn recover(&self) -> bool {
        if self.sockets().lost.is_none() {
            return false;
        }
        self.sockets
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .recover()
    }
}

/// The server's UDP sockets and endpoint. A thread of its own reads every datagram and routes it
/// to its connection's thread. New connections wait in a queue; the host takes them with
/// [`QuicListener::accept`] after the wake it set rings.
pub struct QuicListener {
    demux: Arc<Demux>,
    thread: Option<std::thread::JoinHandle<()>>,
}

struct Demux {
    shared: Arc<Shared>,
    config: Arc<ServerConfig>,
    accepted: Mutex<VecDeque<QuicStream>>,
    wake: Mutex<Option<Arc<dyn crate::connection::Wake>>>,
    /// [`ACCEPTING`], [`RETIRED`] or [`FINISHED`]. One atomic, so a revive and the thread's
    /// finish never both win.
    phase: AtomicU8,
    /// Set by `Drop`: the thread ends at once, even with live connections.
    stopping: AtomicBool,
    widening: Mutex<Widening>,
}

/// A widening the listener's thread performs at its next turn.
enum Widening {
    None,
    /// Answering on the sender.
    Asked(std::sync::mpsc::SyncSender<Result<(), QuicError>>),
    /// The thread ended, so nothing widens.
    Ended,
}

/// The listener takes new connections.
const ACCEPTING: u8 = 0;
/// The listener refuses new connections and routes to its own until the last ends.
const RETIRED: u8 = 1;
/// A retired listener's last connection ended; its thread no longer routes.
const FINISHED: u8 = 2;

impl QuicListener {
    /// Listens at `address`. The unspecified address of either family listens on every
    /// interface, for IPv4 and IPv6.
    pub fn bind(address: SocketAddr, config: Arc<ServerConfig>) -> Result<Self, QuicError> {
        let sockets = Sockets::bind(address)?;
        let endpoint = Endpoint::new(endpoint_config(), Some(Arc::clone(&config)), true);
        let (stopped, stop) = bell()?;
        let demux = Arc::new(Demux {
            shared: Arc::new(Shared {
                server: Mutex::new(Server {
                    endpoint,
                    routes: HashMap::new(),
                    draining: HashMap::new(),
                }),
                sockets: RwLock::new(sockets),
                ring: stop,
            }),
            config,
            accepted: Mutex::default(),
            wake: Mutex::default(),
            phase: AtomicU8::new(ACCEPTING),
            stopping: AtomicBool::new(false),
            widening: Mutex::new(Widening::None),
        });
        let running = Arc::clone(&demux);
        let thread = std::thread::Builder::new()
            .name("spaceterm-quic".into())
            .spawn(move || running.run(&stopped))
            .map_err(|_| QuicError::Transport)?;
        Ok(Self {
            demux,
            thread: Some(thread),
        })
    }

    /// Rings `wake` whenever a new connection waits.
    pub fn set_wake(&self, wake: Arc<dyn crate::connection::Wake>) {
        *self.demux.wake.lock().unwrap_or_else(|e| e.into_inner()) = Some(wake);
    }

    /// The address and port the listener is bound to. Every interface reads as `0.0.0.0`.
    pub fn address(&self) -> SocketAddr {
        self.demux.shared.sockets().address
    }

    /// Widens the listener from one address to every interface at the same port and keeps its
    /// connections, as when Network Access widens. When the wildcard can't be bound the listener keeps
    /// its old address.
    pub fn widen(&self) -> Result<(), QuicError> {
        let (done, result) = std::sync::mpsc::sync_channel(1);
        {
            let mut widening = self
                .demux
                .widening
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if matches!(*widening, Widening::Ended) {
                return Err(QuicError::Transport);
            }
            *widening = Widening::Asked(done);
        }
        // Linux keeps a socket open while a poll waits on it, so the thread closes the old socket
        // between its waits.
        ring(&self.demux.shared.ring);
        result.recv().unwrap_or(Err(QuicError::Transport))
    }

    /// Stops taking connections but keeps routing to the live ones until the last ends, or for
    /// at most [`RETIRE_LIMIT`], so their goodbyes still leave and their acknowledgements still
    /// arrive. Connections that
    /// waited for the host are dropped. Returns false when no connection is live: the caller
    /// drops the listener, which closes its socket at once.
    pub fn retire(&self) -> bool {
        let live = {
            // Under the endpoint lock, so no connection starts between the phase and the check.
            let server = self.demux.shared.lock();
            self.demux.phase.store(RETIRED, Ordering::Release);
            let waiting = self
                .demux
                .accepted
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .len();
            server.routes.len() > waiting
        };
        let waiting = std::mem::take(
            &mut *self
                .demux
                .accepted
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
        );
        drop(waiting);
        ring(&self.demux.shared.ring);
        live
    }

    /// Takes connections again on a retired listener whose thread still routes. Returns false
    /// when its last connection already ended.
    pub fn revive(&self) -> bool {
        self.demux
            .phase
            .compare_exchange(RETIRED, ACCEPTING, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// Whether a retired listener's last connection ended. Dropping it then closes its socket.
    pub fn finished(&self) -> bool {
        self.demux.phase.load(Ordering::Acquire) == FINISHED
    }

    /// Connections waiting for the host to take them.
    pub fn waiting(&self) -> usize {
        self.demux
            .accepted
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    /// The next new connection. Its own thread completes the handshake.
    pub fn accept(&self) -> Option<QuicStream> {
        self.demux
            .accepted
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front()
    }
}

impl Drop for QuicListener {
    /// Ends routing at once, even to live connections.
    fn drop(&mut self) {
        self.demux.stopping.store(true, Ordering::Release);
        ring(&self.demux.shared.ring);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Demux {
    fn run(&self, stopped: &UnixStream) {
        self.serve(stopped);
        // Drops a widening asked meanwhile, which then fails.
        *self.widening.lock().unwrap_or_else(|e| e.into_inner()) = Widening::Ended;
    }

    /// Waits on the sockets and the stop bell, widens when asked, and routes every datagram.
    fn serve(&self, stopped: &UnixStream) {
        let mut buffer = vec![0; RECEIVE_BYTES];
        let mut send = Vec::new();
        let mut descriptors = Vec::with_capacity(3);
        let mut retired_since: Option<Instant> = None;
        loop {
            let poll = |fd| libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            let lost = self.shared.recover();
            descriptors.clear();
            descriptors.push(poll(stopped.as_raw_fd()));
            descriptors.extend(self.shared.sockets().udp.iter().map(|udp| poll(udp.fd())));
            // A retired listener looks for its last connection's end now and then, one that
            // lost an address binds it again, and a draining connection ends at its time.
            let retired = self.phase.load(Ordering::Acquire) == RETIRED;
            let drained = self.shared.lock().next_drained();
            let timeout = [
                (retired || lost).then(|| Duration::from_millis(100)),
                drained.map(|until| until.saturating_duration_since(Instant::now())),
            ]
            .into_iter()
            .flatten()
            .min()
            .map_or(-1, |wait| {
                wait.as_micros().div_ceil(1000).min(i32::MAX as u128) as i32
            });
            // SAFETY: `descriptors` is a live, writable array of `pollfd` for the whole call.
            unsafe {
                libc::poll(
                    descriptors.as_mut_ptr(),
                    descriptors.len() as libc::nfds_t,
                    timeout,
                )
            };
            if descriptors[0].revents != 0 {
                drain_bell(stopped);
            }
            if self.stopping.load(Ordering::Acquire) {
                return;
            }
            let asked = std::mem::replace(
                &mut *self.widening.lock().unwrap_or_else(|e| e.into_inner()),
                Widening::None,
            );
            if let Widening::Asked(done) = asked {
                let _ = done.send(self.shared.widen());
            }
            self.route(&mut buffer, &mut send);
            self.shared.lock().expire(Instant::now());
            retired_since = match (self.phase.load(Ordering::Acquire), retired_since) {
                (RETIRED, None) => Some(Instant::now()),
                (RETIRED, since) => since,
                _ => None,
            };
            let overdue = retired_since.is_some_and(|since| since.elapsed() >= RETIRE_LIMIT);
            if self.try_finish() || (overdue && self.close_retired()) {
                // The host binds again once a retired socket is free.
                if let Some(wake) = &*self.wake.lock().unwrap_or_else(|e| e.into_inner()) {
                    wake.wake();
                }
                return;
            }
        }
    }

    /// Finishes a retired listener past [`RETIRE_LIMIT`] and closes its sockets, while
    /// connections that have not ended still hold it. They can no longer send or receive.
    fn close_retired(&self) -> bool {
        if self
            .phase
            .compare_exchange(RETIRED, FINISHED, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return false;
        }
        let mut sockets = self
            .shared
            .sockets
            .write()
            .unwrap_or_else(|e| e.into_inner());
        sockets.udp.clear();
        sockets.lost = None;
        true
    }

    /// Finishes a retired listener whose last connection ended. A revive that came first keeps
    /// it running; one that comes later finds it finished.
    fn try_finish(&self) -> bool {
        let server = self.shared.lock();
        server.routes.is_empty()
            && server.draining.is_empty()
            && self
                .phase
                .compare_exchange(RETIRED, FINISHED, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
    }

    pub(super) fn send(
        &self,
        bytes: &[u8],
        destination: SocketAddr,
        source: Option<std::net::IpAddr>,
    ) {
        let _ = self.shared.send(bytes, destination, source);
    }

    /// Reads every waiting datagram of each socket and hands it to its connection, or starts a
    /// new one.
    fn route(&self, buffer: &mut [u8], send: &mut Vec<u8>) {
        let mut arrived = false;
        let mut datagrams = Vec::new();
        let mut socket = 0;
        loop {
            datagrams.clear();
            // Handling a datagram sends, so the sockets are read under a lock of their own.
            let received = match self.shared.sockets().udp.get(socket) {
                Some(udp) => udp.receive(buffer, |remote, local, bytes| {
                    datagrams.push((remote, local, BytesMut::from(bytes)));
                }),
                None => break,
            };
            match received {
                Ok(()) => {}
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                // WouldBlock, or a transient error such as an ICMP report: try the next socket,
                // and this one again later.
                Err(_) => {
                    socket += 1;
                    continue;
                }
            }
            for (remote, local, data) in datagrams.drain(..) {
                arrived |= self.handle(remote, local, data, send);
            }
        }
        if arrived && let Some(wake) = &*self.wake.lock().unwrap_or_else(|e| e.into_inner()) {
            wake.wake();
        }
    }

    /// Hands one datagram to its connection, or answers a new one. Returns whether a new
    /// connection now waits for the host.
    fn handle(
        &self,
        remote: SocketAddr,
        local: Option<std::net::IpAddr>,
        data: BytesMut,
        send: &mut Vec<u8>,
    ) -> bool {
        let count = data.len();
        let now = Instant::now();
        let mut server = self.shared.lock();
        send.clear();
        let event = server
            .endpoint
            .handle(now, FourTuple::new(remote, local), None, data, send);
        match event {
            Some(DatagramEvent::ConnectionEvent(handle, event)) => {
                if let Some(route) = server.routes.get(&handle) {
                    if route
                        .events
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push(event, count)
                    {
                        ring(&route.ring);
                    }
                } else {
                    server.answer_draining(handle, event, now, &self.shared, send);
                }
            }
            // A client whose address is not yet proven gets a stateless retry: it must echo
            // the token from that address before it holds any state or slot, so spoofed
            // sources cannot.
            Some(DatagramEvent::NewConnection(incoming))
                if !incoming.remote_address_validated() =>
            {
                match server.endpoint.retry(incoming, send) {
                    Ok(transmit) => self.send(
                        &send[..transmit.size],
                        transmit.destination,
                        transmit.src_ip,
                    ),
                    Err(error) => server.endpoint.ignore(error.into_incoming()),
                }
            }
            Some(DatagramEvent::NewConnection(incoming))
                if self.phase.load(Ordering::Acquire) != ACCEPTING =>
            {
                let transmit = server.endpoint.refuse(incoming, send);
                self.send(
                    &send[..transmit.size],
                    transmit.destination,
                    transmit.src_ip,
                );
            }
            Some(DatagramEvent::NewConnection(incoming)) => {
                let waiting = self
                    .accepted
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .len();
                if waiting >= MAX_PENDING {
                    let transmit = server.endpoint.refuse(incoming, send);
                    self.send(
                        &send[..transmit.size],
                        transmit.destination,
                        transmit.src_ip,
                    );
                    return false;
                }
                match server
                    .endpoint
                    .accept(incoming, now, send, Some(Arc::clone(&self.config)))
                {
                    Ok((handle, connection)) => {
                        let Ok((bell, ringer)) = bell() else {
                            server
                                .endpoint
                                .handle_event(handle, EndpointEvent::drained());
                            return false;
                        };
                        let route = Arc::new(Route {
                            events: Mutex::default(),
                            ring: ringer,
                        });
                        server.routes.insert(handle, Arc::clone(&route));
                        drop(server);
                        self.accepted
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .push_back(QuicStream::new(Driver {
                                connection: ManuallyDrop::new(connection),
                                handle,
                                peer: None,
                                side: Side::Server {
                                    shared: Arc::clone(&self.shared),
                                    route,
                                    bell,
                                },
                                state: DriverState::new(),
                            }));
                        return true;
                    }
                    Err(error) => {
                        if let Some(transmit) = error.response {
                            self.send(
                                &send[..transmit.size],
                                transmit.destination,
                                transmit.src_ip,
                            );
                        }
                    }
                }
            }
            Some(DatagramEvent::Response(transmit)) => {
                self.send(
                    &send[..transmit.size],
                    transmit.destination,
                    transmit.src_ip,
                );
            }
            None => {}
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Datagrams routed to a connection whose thread does not run, as a queued connection's,
    /// stop at the count and byte bounds; taking them makes room again.
    #[test]
    fn routed_datagrams_stop_at_their_count_and_byte_bounds() {
        let mut queue = Bounded::default();
        let mut accepted = 0;
        for index in 0..10 * ROUTED_DATAGRAMS {
            accepted += usize::from(queue.push(index, 1));
        }
        assert_eq!(accepted, ROUTED_DATAGRAMS);
        assert_eq!(queue.take().len(), ROUTED_DATAGRAMS);
        let mut bytes = 0;
        while queue.push(0, DATAGRAM_BYTES * 2) {
            bytes += DATAGRAM_BYTES * 2;
        }
        assert!(bytes <= ROUTED_BYTES && bytes + DATAGRAM_BYTES * 2 > ROUTED_BYTES);
        assert!(queue.take().len() < ROUTED_DATAGRAMS);
        assert!(queue.push(0, DATAGRAM_BYTES));
    }

    fn server_config_for_tests() -> Arc<ServerConfig> {
        let pair = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519).unwrap();
        let certificate = rcgen::CertificateParams::new(vec![MACHINE_NAME.into()])
            .unwrap()
            .self_signed(&pair)
            .unwrap();
        let mut tls = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![certificate.der().clone()],
            rustls::pki_types::PrivateKeyDer::Pkcs8(pair.serialize_der().into()),
        )
        .unwrap();
        tls.alpn_protocols = vec![ALPN.to_vec()];
        server_config(tls).unwrap()
    }

    /// A connection whose thread outlived its drain gave its handle back, and the endpoint may
    /// have given that handle to a new connection. Handing the old one over keeps the new
    /// route.
    #[test]
    fn a_drained_connection_leaves_a_reused_handle_routed() {
        let now = Instant::now();
        let mut tls = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_root_certificates(rustls::RootCertStore::empty())
        .with_no_client_auth();
        tls.alpn_protocols = vec![ALPN.to_vec()];
        let mut client = Endpoint::new(endpoint_config(), None, true);
        let (handle, mut connection) = client
            .connect(
                now,
                client_config(tls).unwrap(),
                "127.0.0.1:9".parse().unwrap(),
                MACHINE_NAME,
            )
            .unwrap();
        connection.close(now, VarInt::from_u32(0), bytes::Bytes::new());
        connection.handle_timeout(now + Duration::from_secs(60));
        assert!(connection.is_drained());

        let (_, ringer) = bell().unwrap();
        let mut server = Server {
            endpoint: Endpoint::new(endpoint_config(), Some(server_config_for_tests()), true),
            routes: HashMap::from([(
                handle,
                Arc::new(Route {
                    events: Mutex::default(),
                    ring: ringer,
                }),
            )]),
            draining: HashMap::new(),
        };
        server.drain(handle, connection, now);
        assert!(server.routes.contains_key(&handle), "the new route stays");
        assert!(server.draining.is_empty());
    }

    /// A revive and the thread's finish race for a retired listener. Exactly one wins: a
    /// revived listener never ends up finished, and a finished one is never revived.
    #[test]
    fn a_revive_and_a_finish_never_both_win() {
        let config = server_config_for_tests();
        let (mut revived, mut finished) = (0, 0);
        for _ in 0..50 {
            let listener =
                QuicListener::bind("127.0.0.1:0".parse().unwrap(), Arc::clone(&config)).unwrap();
            // With no connection the thread finishes at its next turn, racing this revive.
            listener.demux.phase.store(RETIRED, Ordering::Release);
            ring(&listener.demux.shared.ring);
            let won = listener.revive();
            let settled = Instant::now() + Duration::from_millis(150);
            while Instant::now() < settled {
                std::thread::park_timeout(settled - Instant::now());
            }
            assert_ne!(won, listener.finished());
            if won {
                revived += 1;
            } else {
                finished += 1;
            }
        }
        assert_eq!(revived + finished, 50);
    }

    /// The address the system would send from to reach the internet, when it has a route.
    fn routed(ipv4: bool) -> Option<std::net::IpAddr> {
        let (local, remote): (SocketAddr, SocketAddr) = if ipv4 {
            ("0.0.0.0:0".parse().unwrap(), "192.0.2.1:9".parse().unwrap())
        } else {
            (
                "[::]:0".parse().unwrap(),
                "[2001:db8::1]:9".parse().unwrap(),
            )
        };
        let socket = UdpSocket::bind(local).ok()?;
        socket.connect(remote).ok()?;
        Some(socket.local_addr().ok()?.ip()).filter(|ip| !ip.is_loopback())
    }

    /// Sockets on every interface answer each datagram from the address it reached, also when
    /// the system would pick another source to reach the client. A client that dialed one of the
    /// server's addresses never sees a reply from another, and a connected client drops one.
    #[test]
    fn every_interface_replies_from_the_address_each_client_dialed() {
        let sockets = Sockets::bind("0.0.0.0:0".parse().unwrap()).unwrap();
        let port = sockets.udp[0].port().unwrap();
        assert_eq!(sockets.udp.len(), 2, "one socket per family");
        assert_eq!(sockets.udp[1].port(), Some(port));
        let loopback: std::net::IpAddr = std::net::Ipv4Addr::LOCALHOST.into();
        // Each client is bound to one address and dials another of the same machine, so the
        // system's own choice of source for the reply is the client's address, not the dialed
        // one. Linux routes all of 127.0.0.0/8 to itself; other systems use the routed address.
        let mut cases = vec![
            (loopback, loopback),
            (
                std::net::Ipv6Addr::LOCALHOST.into(),
                std::net::Ipv6Addr::LOCALHOST.into(),
            ),
        ];
        if cfg!(target_os = "linux") {
            cases.push((loopback, std::net::Ipv4Addr::new(127, 0, 0, 2).into()));
        }
        // On macOS the routed IPv4 cases take `send_from`, the only source macOS honors.
        if let Some(routed) = routed(true) {
            cases.push((loopback, routed));
            cases.push((routed, loopback));
        }
        if let Some(routed) = routed(false) {
            let loopback = std::net::Ipv6Addr::LOCALHOST.into();
            cases.push((loopback, routed));
            cases.push((routed, loopback));
        }
        assert!(
            cases
                .iter()
                .any(|(client, dialed)| client != dialed && dialed.is_ipv4()),
            "no IPv4 address where the system would answer from another source: Linux has \
             127.0.0.2, other systems need a routed address"
        );
        let mut buffer = vec![0; RECEIVE_BYTES];
        for (client, dialed) in cases {
            let client = UdpSocket::bind((client, 0)).unwrap();
            client
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            client.send_to(b"ping", (dialed, port)).unwrap();
            let udp = sockets
                .for_destination(SocketAddr::new(dialed, port))
                .unwrap();
            let mut descriptor = libc::pollfd {
                fd: udp.fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: `descriptor` is a live, writable `pollfd` for the whole call.
            unsafe { libc::poll(&mut descriptor, 1, 5000) };
            let mut arrived = None;
            let _ = udp.receive(&mut buffer, |remote, local, _| {
                arrived = Some((remote, local));
            });
            let (remote, local) = arrived.unwrap_or_else(|| panic!("nothing reached {dialed}"));
            assert_eq!(local, Some(dialed));
            udp.send(b"pong", remote, local).unwrap();
            let (_, from) = client.recv_from(&mut buffer).unwrap();
            assert_eq!(from, SocketAddr::new(dialed, port), "the reply's source");
        }
    }

    /// A listener widens from one address to every interface at the same port. A port taken in
    /// the other family leaves it at the address it had, with its socket.
    #[test]
    fn widening_to_every_interface_keeps_the_old_address_when_the_port_is_taken() {
        let config = server_config_for_tests();
        // Another test can hold the port at another address, so a taken port tries another.
        let listener = (0..8)
            .find_map(|_| {
                let listener =
                    QuicListener::bind("127.0.0.1:0".parse().unwrap(), Arc::clone(&config))
                        .unwrap();
                listener.widen().ok().map(|()| listener)
            })
            .expect("a port free on every interface");
        let port = listener.address().port();
        assert_eq!(listener.address(), SocketAddr::from(([0, 0, 0, 0], port)));
        assert_eq!(listener.demux.shared.sockets().udp.len(), 2);
        assert_eq!(
            listener.widen(),
            Err(QuicError::Configuration),
            "already wide"
        );

        let listener =
            QuicListener::bind("127.0.0.1:0".parse().unwrap(), Arc::clone(&config)).unwrap();
        let address = listener.address();
        let taken = UdpSocket::bind((std::net::Ipv6Addr::LOCALHOST, address.port())).unwrap();
        assert_eq!(listener.widen(), Err(QuicError::AddressInUse));
        assert_eq!(listener.address(), address);
        assert_eq!(listener.demux.shared.sockets().udp.len(), 1);
        drop(taken);
    }

    /// A server connection whose datagram the listener's full socket refused waits on a deadline,
    /// never on the listener's socket, so the listener can still widen while it waits.
    #[test]
    fn a_machine_connection_with_a_refused_datagram_lets_its_listener_widen() {
        let config = server_config_for_tests();
        let listener =
            QuicListener::bind("127.0.0.1:0".parse().unwrap(), Arc::clone(&config)).unwrap();
        let address = listener.address();
        let client_config = {
            let mut tls = rustls::ClientConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AnyCore))
            .with_no_client_auth();
            tls.alpn_protocols = vec![ALPN.to_vec()];
            client_config(tls).unwrap()
        };
        let client = std::thread::spawn(move || {
            QuicStream::connect(
                client_config,
                &[address],
                Instant::now() + Duration::from_secs(5),
                &AtomicBool::new(false),
            )
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut stream = loop {
            if let Some(stream) = listener.accept() {
                break stream;
            }
            assert!(Instant::now() < deadline, "no connection arrived");
            let mut bell = libc::pollfd {
                fd: listener.demux.shared.sockets().udp[0].fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: `bell` is a live, writable `pollfd` for the whole call.
            unsafe { libc::poll(&mut bell, 1, 10) };
        };
        stream.handshake(deadline).unwrap();
        // As when the socket's send buffer is full.
        stream.driver.state.stashed = Some((address, None, vec![0; 16]));
        stream.driver.state.retry = Some(Instant::now() + Duration::from_secs(60));
        assert_eq!(stream.driver.stashed_descriptor(), None);
        assert!(stream.driver.stash_deadline().is_some());
        let carrier = stream.into_carrier();
        assert_eq!(carrier.readiness().write, None);
        let waiting = std::thread::spawn(move || {
            let _ = carrier
                .driver
                .wait(Instant::now() + Duration::from_millis(500), None);
            carrier
        });
        assert_eq!(listener.widen(), Ok(()));
        assert_eq!(listener.demux.shared.sockets().udp.len(), 2);
        drop(waiting.join().unwrap());
        // The client waits for bytes on L0 that never come.
        let _ = client.join();
    }

    /// The paired transport reads the clock before each write on L0: a frame whose deadline
    /// passed writes nothing, and one whose deadline passes while its peer reads nothing says
    /// that part of it left.
    #[test]
    fn a_quic_frame_writes_nothing_after_its_deadline_and_says_when_part_of_it_left() {
        use crate::admission::{Preamble, Unsent};
        let config = server_config_for_tests();
        let listener =
            QuicListener::bind("127.0.0.1:0".parse().unwrap(), Arc::clone(&config)).unwrap();
        let address = listener.address();
        let client_config = {
            let mut tls = rustls::ClientConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AnyCore))
            .with_no_client_auth();
            tls.alpn_protocols = vec![ALPN.to_vec()];
            client_config(tls).unwrap()
        };
        let client = std::thread::spawn(move || {
            QuicStream::connect(
                client_config,
                &[address],
                Instant::now() + Duration::from_secs(5),
                &AtomicBool::new(false),
            )
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut stream = loop {
            if let Some(stream) = listener.accept() {
                break stream;
            }
            assert!(Instant::now() < deadline, "no connection arrived");
            let mut bell = libc::pollfd {
                fd: listener.demux.shared.sockets().udp[0].fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: `bell` is a live, writable `pollfd` for the whole call.
            unsafe { libc::poll(&mut bell, 1, 10) };
        };
        stream.handshake(deadline).unwrap();
        assert_eq!(
            stream.send_frame(b"{\"request\":\"execute\"}", Instant::now()),
            Err(Unsent {
                error: crate::CoreError::Protocol,
                written: false,
            })
        );
        // The client takes L0 only once its first byte arrives, and no STREAM frame left.
        stream.driver.pump();
        assert_eq!(stream.driver.connection.stats().frame_tx.stream, 0);
        assert!(!client.is_finished());
        // Far more than the client's receive window, which nobody reads.
        let frame = vec![0; 512 * 1024];
        let began = Instant::now();
        let unsent = stream
            .send_frame(&frame, began + Duration::from_millis(300))
            .unwrap_err();
        assert!(unsent.written);
        assert!(
            began.elapsed() < Duration::from_secs(2),
            "{:?}",
            began.elapsed()
        );
        drop(stream);
        let _ = client.join();
    }

    /// Accepts any server certificate, for tests of the transport alone.
    #[derive(Debug)]
    struct AnyCore;

    impl rustls::client::danger::ServerCertVerifier for AnyCore {
        fn verify_server_cert(
            &self,
            _: &rustls::pki_types::CertificateDer<'_>,
            _: &[rustls::pki_types::CertificateDer<'_>],
            _: &rustls::pki_types::ServerName<'_>,
            _: &[u8],
            _: rustls::pki_types::UnixTime,
        ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        }
        fn verify_tls12_signature(
            &self,
            _: &[u8],
            _: &rustls::pki_types::CertificateDer<'_>,
            _: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
        }
        fn verify_tls13_signature(
            &self,
            _: &[u8],
            _: &rustls::pki_types::CertificateDer<'_>,
            _: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
        }
        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
            rustls::crypto::ring::default_provider()
                .signature_verification_algorithms
                .supported_schemes()
        }
    }

    /// A retired listener without connections reports none, so its owner drops it and the
    /// port is free at once.
    #[test]
    fn retiring_an_idle_listener_frees_its_port_when_dropped() {
        let config = server_config_for_tests();
        let listener =
            QuicListener::bind("127.0.0.1:0".parse().unwrap(), Arc::clone(&config)).unwrap();
        let bound = listener.demux.shared.sockets().udp[0]
            .socket
            .local_addr()
            .unwrap();
        assert!(!listener.retire(), "no connection is live");
        drop(listener);
        assert!(QuicListener::bind(bound, config).is_ok());
    }

    /// An address this computer does not hold yet, such as a Tailscale address before tailscaled
    /// assigns it, is a refusal of its own, so the server can wait for the address to appear.
    #[test]
    fn binding_an_address_this_computer_lacks_reports_it_unassigned() {
        let documentation = "192.0.2.77:0".parse().unwrap();
        assert!(matches!(
            QuicListener::bind(documentation, server_config_for_tests()),
            Err(QuicError::AddressNotAssigned)
        ));
    }

    /// A connection whose thread holds it past retirement, as a severed lane still running a
    /// download does, keeps a retired listener routing for at most [`RETIRE_LIMIT`]. Its socket
    /// then closes, so the address binds again.
    #[test]
    fn a_retired_listener_frees_its_port_within_the_limit_while_a_connection_lives() {
        let config = server_config_for_tests();
        let listener =
            QuicListener::bind("127.0.0.1:0".parse().unwrap(), Arc::clone(&config)).unwrap();
        let address = listener.address();
        let client_config = {
            let mut tls = rustls::ClientConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AnyCore))
            .with_no_client_auth();
            tls.alpn_protocols = vec![ALPN.to_vec()];
            client_config(tls).unwrap()
        };
        let client = std::thread::spawn(move || {
            QuicStream::connect(
                client_config,
                &[address],
                Instant::now() + Duration::from_secs(8),
                &AtomicBool::new(false),
            )
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut held = loop {
            if let Some(stream) = listener.accept() {
                break stream;
            }
            assert!(Instant::now() < deadline, "no connection arrived");
            std::thread::park_timeout(Duration::from_millis(5));
        };
        held.handshake(deadline).unwrap();
        let retired = Instant::now();
        assert!(listener.retire(), "the held connection is live");
        assert!(!listener.finished());
        while !listener.finished() {
            assert!(
                retired.elapsed() < RETIRE_LIMIT + Duration::from_secs(2),
                "the retired listener outlived its limit"
            );
            std::thread::park_timeout(Duration::from_millis(20));
        }
        assert!(retired.elapsed() >= RETIRE_LIMIT);
        // The connection still lives, and its address binds again.
        assert!(QuicListener::bind(address, config).is_ok());
        drop(held);
        // The client waits for bytes on L0 that never come.
        let _ = client.join();
    }
}
