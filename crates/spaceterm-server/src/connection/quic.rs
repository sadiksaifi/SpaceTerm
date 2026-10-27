//! The QUIC carrier: noq-proto, n0's fork of quinn's protocol state machine, driven on the
//! connection's own I/O thread.
//!
//! - **No runtime.** noq-proto does no I/O. Each connection's thread reads and writes UDP itself,
//!   so a frame crosses no other thread on the client. On the server one UDP socket carries every
//!   connection: a thread of the listener reads it and hands each datagram to its connection's
//!   thread, which sends its own datagrams directly.
//! - **Pinned keys.** TLS 1.3 runs inside QUIC with the same pinned server certificate as before.
//!   The client presents a self-signed certificate of its device key, which the server compares
//!   with the enrolled key. The callers build both rustls configurations.
//! - **Streams.** L0 is the bidirectional stream the server opens for pairing and admission. The
//!   server opens one unidirectional stream for L1 and one for L2 when it first sends at that
//!   level; such a stream starts with its level byte. Frames follow back to back with their length
//!   prefix, as on the Unix socket. A lost packet stalls only its own level.
//! - **Ordering.** Frames of one level arrive in the order they were offered. The client sends
//!   every level on L0, so a cancellation or an edit acknowledgement never overtakes its request.
//!   On close the server finishes all three streams, opening any it never used, and the client
//!   holds the server's goodbye until L1 and L2 have ended, so a goodbye never overtakes frames
//!   in flight.
//! - **Readiness.** The carrier reports its socket or bell as readable when datagrams arrive, and
//!   QUIC's next timer as a deadline. Write progress needs incoming acknowledgements, so the same
//!   descriptor is the progress descriptor.
use super::carrier::{Carrier, CarrierError, Flush, Offer, Priority, Readiness};
use crate::protocol::frame::FrameReader;
use crate::protocol::{RawFrame, limits};
use bytes::BytesMut;
use noq_proto::congestion::{Controller, ControllerFactory, CubicConfig};
use noq_proto::{
    AckFrequencyConfig, ClientConfig, Connection, ConnectionError, ConnectionEvent,
    ConnectionHandle, DatagramEvent, Dir, Endpoint, EndpointConfig, EndpointEvent, Event,
    FourTuple, RttEstimator, ServerConfig, StreamEvent, StreamId, TransportConfig, VarInt,
};
use std::collections::{HashMap, VecDeque};
use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, UdpSocket};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// The protocol's own ALPN.
pub const ALPN: &[u8] = b"spaceterm/2";
/// Received frame bytes that `receive` has not taken yet. Above it the carrier stops reading
/// streams, so the peer's sending waits on QUIC flow control.
const INBOUND_BYTES: usize = 2 * limits::MAX_FRAME_BYTES;
/// Largest UDP payload read at once.
const DATAGRAM_BYTES: usize = 65_536;
/// Connections whose handshake started and that the host has not taken yet.
const MAX_PENDING: usize = 32;
/// The server name every client sends. The pinned certificate, not a name, authenticates.
pub const SERVER_NAME: &str = "spaceterm.invalid";

/// Failures before the carrier exists. Classifications only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum QuicError {
    #[error("the QUIC configuration is invalid")]
    Configuration,
    #[error("the QUIC connection failed")]
    Transport,
    #[error("the QUIC connection timed out")]
    TimedOut,
    #[error("the peer closed the QUIC connection")]
    Closed,
}

fn transport() -> Arc<TransportConfig> {
    // Terminal traffic is a few packets at a time, so most losses are tail losses that only a
    // probe timeout recovers. Asking the peer to acknowledge every packet at once keeps its
    // acknowledgement delay out of that timeout.
    let mut acknowledgements = AckFrequencyConfig::default();
    acknowledgements
        .ack_eliciting_threshold(VarInt::from_u32(0))
        .max_ack_delay(Some(Duration::from_millis(1)));
    let mut config = TransportConfig::default();
    config
        // The server opens the one L0 stream; each side opens one stream per other level.
        .max_concurrent_bidi_streams(VarInt::from_u32(1))
        .max_concurrent_uni_streams(VarInt::from_u32(2))
        .ack_frequency_config(Some(acknowledgements))
        .congestion_controller_factory(Arc::new(Floored(Arc::default())))
        // Routers forget idle UDP flows after about 30 seconds.
        .keep_alive_interval(Some(Duration::from_secs(10)));
    Arc::new(config)
}

/// A client configuration from rustls, which must offer [`ALPN`].
pub fn client_config(tls: rustls::ClientConfig) -> Result<ClientConfig, QuicError> {
    let crypto = noq_proto::crypto::rustls::QuicClientConfig::try_from(Arc::new(tls))
        .map_err(|_| QuicError::Configuration)?;
    let mut config = ClientConfig::new(Arc::new(crypto));
    config.transport_config(transport());
    Ok(config)
}

/// A server configuration from rustls, which must offer [`ALPN`].
pub fn server_config(tls: rustls::ServerConfig) -> Result<Arc<ServerConfig>, QuicError> {
    let crypto = noq_proto::crypto::rustls::QuicServerConfig::try_from(Arc::new(tls))
        .map_err(|_| QuicError::Configuration)?;
    let mut config = ServerConfig::with_crypto(Arc::new(crypto));
    config.transport_config(transport());
    Ok(Arc::new(config))
}

/// Packets the congestion window always allows.
const FLOOR_PACKETS: u64 = 10;

/// Builds Cubic with a window of at least [`FLOOR_PACKETS`]. Cubic grows only while the window
/// limits sending, and terminal traffic rarely fills it, so random loss alone ratchets it down to
/// two packets and a screen of output then waits a round trip. The floor equals the usual initial
/// window.
#[derive(Debug)]
struct Floored(Arc<CubicConfig>);

impl ControllerFactory for Floored {
    fn build(self: Arc<Self>, now: Instant, current_mtu: u16) -> Box<dyn Controller> {
        Box::new(Floor {
            inner: Arc::clone(&self.0).build(now, current_mtu),
            mtu: current_mtu,
        })
    }
}

#[derive(Debug)]
struct Floor {
    inner: Box<dyn Controller>,
    mtu: u16,
}

impl Controller for Floor {
    fn on_sent(&mut self, now: Instant, bytes: u64, largest: u64) {
        self.inner.on_sent(now, bytes, largest);
    }
    fn on_packet_sent(&mut self, now: Instant, bytes: u16, packet: u64) {
        self.inner.on_packet_sent(now, bytes, packet);
    }
    fn on_cwnd_limited(&mut self) {
        self.inner.on_cwnd_limited();
    }
    fn on_ack(
        &mut self,
        now: Instant,
        sent: Instant,
        bytes: u64,
        packet: u64,
        app_limited: bool,
        rtt: &RttEstimator,
    ) {
        self.inner
            .on_ack(now, sent, bytes, packet, app_limited, rtt);
    }
    fn on_end_acks(
        &mut self,
        now: Instant,
        in_flight: u64,
        app_limited: bool,
        largest: Option<u64>,
    ) {
        self.inner.on_end_acks(now, in_flight, app_limited, largest);
    }
    fn on_congestion_event(
        &mut self,
        now: Instant,
        sent: Instant,
        persistent: bool,
        ecn: bool,
        lost: u64,
        largest: u64,
    ) {
        self.inner
            .on_congestion_event(now, sent, persistent, ecn, lost, largest);
    }
    fn on_packet_lost(&mut self, bytes: u16, packet: u64, now: Instant) {
        self.inner.on_packet_lost(bytes, packet, now);
    }
    fn on_spurious_congestion_event(&mut self) {
        self.inner.on_spurious_congestion_event();
    }
    fn on_mtu_update(&mut self, mtu: u16) {
        self.mtu = mtu;
        self.inner.on_mtu_update(mtu);
    }
    fn on_ack_frequency_update(&mut self, threshold: u64, delay: Duration) {
        self.inner.on_ack_frequency_update(threshold, delay);
    }
    fn window(&self) -> u64 {
        self.inner.window().max(FLOOR_PACKETS * u64::from(self.mtu))
    }
    fn clone_box(&self) -> Box<dyn Controller> {
        Box::new(Self {
            inner: self.inner.clone_box(),
            mtu: self.mtu,
        })
    }
    fn initial_window(&self) -> u64 {
        self.inner.initial_window()
    }
    fn into_any(self: Box<Self>) -> Box<dyn std::any::Any> {
        self
    }
}

fn nonblocking_socket(address: SocketAddr) -> Result<UdpSocket, QuicError> {
    let socket = UdpSocket::bind(address).map_err(|_| QuicError::Transport)?;
    socket
        .set_nonblocking(true)
        .map_err(|_| QuicError::Transport)?;
    Ok(socket)
}

fn bell() -> Result<(UnixStream, UnixStream), QuicError> {
    let (bell, ring) = UnixStream::pair().map_err(|_| QuicError::Transport)?;
    bell.set_nonblocking(true)
        .and_then(|()| ring.set_nonblocking(true))
        .map_err(|_| QuicError::Transport)?;
    Ok((bell, ring))
}

fn ring(ring: &UnixStream) {
    // A full socket already wakes the thread, so a refused byte is harmless.
    let _ = (&*ring).write(&[1]);
}

fn drain_bell(bell: &UnixStream) {
    let mut bytes = [0; 64];
    while matches!((&*bell).read(&mut bytes), Ok(count) if count > 0) {}
}

/// The server's endpoint and the connections it routes datagrams to.
struct Server {
    endpoint: Endpoint,
    routes: HashMap<ConnectionHandle, Arc<Route>>,
}

/// Datagrams for one server connection, waiting for its thread.
struct Route {
    events: Mutex<VecDeque<ConnectionEvent>>,
    ring: UnixStream,
}

struct Shared {
    server: Mutex<Server>,
    socket: Arc<UdpSocket>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Server> {
        self.server.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// The server's UDP socket and endpoint. A thread of its own reads every datagram and routes it
/// to its connection's thread. New connections wait in a queue; the host takes them with
/// [`QuicListener::accept`] after the wake it set rings.
pub struct QuicListener {
    demux: Arc<Demux>,
    stop: UnixStream,
    thread: Option<std::thread::JoinHandle<()>>,
}

struct Demux {
    shared: Arc<Shared>,
    config: Arc<ServerConfig>,
    accepted: Mutex<VecDeque<QuicStream>>,
    wake: Mutex<Option<Arc<dyn super::Wake>>>,
}

impl QuicListener {
    pub fn bind(address: SocketAddr, config: Arc<ServerConfig>) -> Result<Self, QuicError> {
        let socket = Arc::new(nonblocking_socket(address)?);
        let endpoint = Endpoint::new(
            Arc::new(EndpointConfig::default()),
            Some(Arc::clone(&config)),
            true,
        );
        let demux = Arc::new(Demux {
            shared: Arc::new(Shared {
                server: Mutex::new(Server {
                    endpoint,
                    routes: HashMap::new(),
                }),
                socket,
            }),
            config,
            accepted: Mutex::default(),
            wake: Mutex::default(),
        });
        let (stopped, stop) = bell()?;
        let running = Arc::clone(&demux);
        let thread = std::thread::Builder::new()
            .name("spaceterm-quic".into())
            .spawn(move || running.run(&stopped))
            .map_err(|_| QuicError::Transport)?;
        Ok(Self {
            demux,
            stop,
            thread: Some(thread),
        })
    }

    /// Rings `wake` whenever a new connection waits.
    pub fn set_wake(&self, wake: Arc<dyn super::Wake>) {
        *self.demux.wake.lock().unwrap_or_else(|e| e.into_inner()) = Some(wake);
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
    fn drop(&mut self) {
        ring(&self.stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Demux {
    /// Waits on the socket and the stop bell, and routes every datagram.
    fn run(&self, stopped: &UnixStream) {
        let mut buffer = vec![0; DATAGRAM_BYTES];
        let mut send = Vec::new();
        loop {
            let mut descriptors = [
                libc::pollfd {
                    fd: self.shared.socket.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: stopped.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            // SAFETY: `descriptors` is a live, writable array of `pollfd` for the whole call.
            unsafe { libc::poll(descriptors.as_mut_ptr(), 2, -1) };
            if descriptors[1].revents != 0 {
                return;
            }
            self.route(&mut buffer, &mut send);
        }
    }

    fn send(&self, bytes: &[u8], destination: SocketAddr) {
        let _ = self.shared.socket.send_to(bytes, destination);
    }

    /// Reads every waiting datagram and hands it to its connection, or starts a new one.
    fn route(&self, buffer: &mut [u8], send: &mut Vec<u8>) {
        let mut arrived = false;
        loop {
            let (count, remote) = match self.shared.socket.recv_from(buffer) {
                Ok(received) => received,
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                // WouldBlock, or a transient error such as an ICMP report: try again later.
                Err(_) => break,
            };
            let now = Instant::now();
            let data = BytesMut::from(&buffer[..count]);
            let mut server = self.shared.lock();
            send.clear();
            let event = server
                .endpoint
                .handle(now, FourTuple::new(remote, None), None, data, send);
            match event {
                Some(DatagramEvent::ConnectionEvent(handle, event)) => {
                    if let Some(route) = server.routes.get(&handle) {
                        route
                            .events
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .push_back(event);
                        ring(&route.ring);
                    }
                }
                Some(DatagramEvent::NewConnection(incoming)) => {
                    let waiting = self
                        .accepted
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .len();
                    if waiting >= MAX_PENDING {
                        let transmit = server.endpoint.refuse(incoming, send);
                        self.send(&send[..transmit.size], transmit.destination);
                        continue;
                    }
                    match server.endpoint.accept(
                        incoming,
                        now,
                        send,
                        Some(Arc::clone(&self.config)),
                    ) {
                        Ok((handle, connection)) => {
                            let Ok((bell, ringer)) = bell() else {
                                server
                                    .endpoint
                                    .handle_event(handle, EndpointEvent::drained());
                                continue;
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
                                    connection,
                                    handle,
                                    socket: Arc::clone(&self.shared.socket),
                                    peer: None,
                                    side: Side::Server {
                                        shared: Arc::clone(&self.shared),
                                        route,
                                        bell,
                                    },
                                    state: DriverState::new(),
                                }));
                            arrived = true;
                        }
                        Err(error) => {
                            if let Some(transmit) = error.response {
                                self.send(&send[..transmit.size], transmit.destination);
                            }
                        }
                    }
                }
                Some(DatagramEvent::Response(transmit)) => {
                    self.send(&send[..transmit.size], transmit.destination);
                }
                None => {}
            }
        }
        if arrived && let Some(wake) = &*self.wake.lock().unwrap_or_else(|e| e.into_inner()) {
            wake.wake();
        }
    }
}

/// Where a connection's datagrams come from.
enum Side {
    /// A client owns its socket and endpoint.
    Client {
        endpoint: Box<Endpoint>,
        buffer: Vec<u8>,
    },
    /// A server connection receives what the listener routes to it, and rings its bell.
    Server {
        shared: Arc<Shared>,
        route: Arc<Route>,
        bell: UnixStream,
    },
}

/// What the connection's events reported.
struct DriverState {
    connected: bool,
    lost: Option<CarrierError>,
    /// Send streams whose data the peer acknowledged after this end finished them.
    finished: usize,
    /// A datagram that would have blocked, retried before any other.
    stashed: Option<(SocketAddr, Vec<u8>)>,
    transmit: Vec<u8>,
}

impl DriverState {
    fn new() -> Self {
        Self {
            connected: false,
            lost: None,
            finished: 0,
            stashed: None,
            transmit: Vec::new(),
        }
    }
}

/// Sends one datagram. A client's socket is connected to its server, which keeps the kernel's
/// route lookup off every send; macOS refuses an explicit address on a connected socket.
fn send(
    socket: &UdpSocket,
    peer: Option<SocketAddr>,
    bytes: &[u8],
    destination: SocketAddr,
) -> std::io::Result<usize> {
    if peer == Some(destination) {
        socket.send(bytes)
    } else {
        socket.send_to(bytes, destination)
    }
}

/// One QUIC connection and its I/O, driven only when its thread calls it.
struct Driver {
    connection: Connection,
    handle: ConnectionHandle,
    socket: Arc<UdpSocket>,
    /// The server a client's socket is connected to.
    peer: Option<SocketAddr>,
    side: Side,
    state: DriverState,
}

impl Driver {
    /// The descriptor that becomes readable when datagrams arrive for this connection.
    fn descriptor(&self) -> RawFd {
        match &self.side {
            Side::Client { .. } => self.socket.as_raw_fd(),
            Side::Server { bell, .. } => bell.as_raw_fd(),
        }
    }

    /// Takes arrived datagrams, runs expired timers, exchanges endpoint events, records the
    /// connection's events and sends what it has.
    fn pump(&mut self) {
        let now = Instant::now();
        self.ingest(now);
        self.advance(now);
    }

    /// Like [`Self::pump`] without taking datagrams, so the descriptor stays readable for the
    /// I/O thread's next `receive`.
    fn advance(&mut self, now: Instant) {
        if self
            .connection
            .poll_timeout()
            .is_some_and(|deadline| deadline <= now)
        {
            self.connection.handle_timeout(now);
        }
        self.endpoint_events();
        while let Some(event) = self.connection.poll() {
            match event {
                Event::Connected => self.state.connected = true,
                Event::ConnectionLost { reason } => {
                    self.state.lost.get_or_insert(match reason {
                        ConnectionError::ApplicationClosed(_)
                        | ConnectionError::ConnectionClosed(_)
                        | ConnectionError::LocallyClosed => CarrierError::Closed,
                        _ => CarrierError::Failed,
                    });
                }
                Event::Stream(StreamEvent::Finished { .. }) => self.state.finished += 1,
                _ => {}
            }
        }
        self.transmit(now);
    }

    fn ingest(&mut self, now: Instant) {
        match &mut self.side {
            Side::Client { endpoint, buffer } => loop {
                let (count, remote) = match self.socket.recv_from(buffer) {
                    Ok(received) => received,
                    Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                    // The connected socket reports the server's port as closed, as TCP would.
                    Err(error)
                        if error.kind() == ErrorKind::ConnectionRefused
                            && !self.state.connected =>
                    {
                        self.state.lost.get_or_insert(CarrierError::Failed);
                        break;
                    }
                    Err(_) => break,
                };
                let data = BytesMut::from(&buffer[..count]);
                self.state.transmit.clear();
                match endpoint.handle(
                    now,
                    FourTuple::new(remote, None),
                    None,
                    data,
                    &mut self.state.transmit,
                ) {
                    Some(DatagramEvent::ConnectionEvent(handle, event))
                        if handle == self.handle =>
                    {
                        self.connection.handle_event(event);
                    }
                    Some(DatagramEvent::Response(transmit)) => {
                        let _ = send(
                            &self.socket,
                            self.peer,
                            &self.state.transmit[..transmit.size],
                            transmit.destination,
                        );
                    }
                    _ => {}
                }
            },
            Side::Server { route, bell, .. } => {
                drain_bell(bell);
                let events: Vec<_> = route
                    .events
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .drain(..)
                    .collect();
                for event in events {
                    self.connection.handle_event(event);
                }
            }
        }
    }

    fn endpoint_events(&mut self) {
        while let Some(event) = self.connection.poll_endpoint_events() {
            let reply = match &mut self.side {
                Side::Client { endpoint, .. } => endpoint.handle_event(self.handle, event),
                Side::Server { shared, .. } => {
                    let drained = event.is_drained();
                    let mut server = shared.lock();
                    let reply = server.endpoint.handle_event(self.handle, event);
                    if drained {
                        server.routes.remove(&self.handle);
                    }
                    reply
                }
            };
            if let Some(event) = reply {
                self.connection.handle_event(event);
            }
        }
    }

    /// Sends datagrams until QUIC has none or the socket would block.
    fn transmit(&mut self, now: Instant) {
        if let Some((destination, bytes)) = self.state.stashed.take() {
            match send(&self.socket, self.peer, &bytes, destination) {
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    self.state.stashed = Some((destination, bytes));
                    return;
                }
                _ => {}
            }
        }
        loop {
            self.state.transmit.clear();
            let Some(transmit) = self.connection.poll_transmit(
                now,
                std::num::NonZeroUsize::MIN,
                &mut self.state.transmit,
            ) else {
                return;
            };
            let bytes = &self.state.transmit[..transmit.size];
            match send(&self.socket, self.peer, bytes, transmit.destination) {
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    self.state.stashed = Some((transmit.destination, bytes.to_vec()));
                    return;
                }
                // The connected socket's local address left with its network: move to a socket
                // the system binds afresh. QUIC retransmits what this send lost.
                Err(error)
                    if self.peer.is_some()
                        && matches!(
                            error.kind(),
                            ErrorKind::AddrNotAvailable
                                | ErrorKind::NetworkUnreachable
                                | ErrorKind::HostUnreachable
                                | ErrorKind::NetworkDown
                        ) =>
                {
                    self.rebind(now);
                    return;
                }
                // A lost datagram is QUIC's to recover.
                _ => {}
            }
        }
    }

    /// Moves a client to a new UDP socket, as after a switch between networks. The server
    /// validates the new path and the connection continues without a new handshake.
    fn rebind(&mut self, now: Instant) {
        let Some(peer) = self.peer else {
            return;
        };
        let local: SocketAddr = if peer.is_ipv4() {
            (std::net::Ipv4Addr::UNSPECIFIED, 0).into()
        } else {
            (std::net::Ipv6Addr::UNSPECIFIED, 0).into()
        };
        let Ok(socket) = nonblocking_socket(local) else {
            return;
        };
        if socket.connect(peer).is_err() {
            return;
        }
        self.socket = Arc::new(socket);
        self.state.stashed = None;
        self.connection.handle_network_change(None, now);
        self.transmit(now);
    }

    /// Waits for datagrams, QUIC's next timer or `deadline`, whichever comes first.
    fn wait(&self, deadline: Instant) -> Result<(), QuicError> {
        let now = Instant::now();
        if now >= deadline {
            return Err(QuicError::TimedOut);
        }
        let until = self
            .connection
            .poll_timeout()
            .map_or(deadline, |timer| timer.min(deadline));
        let _ = super::io::wait_fd(self.descriptor(), self.state.stashed.is_some(), until);
        Ok(())
    }

    fn check(&self) -> Result<(), QuicError> {
        match self.state.lost {
            Some(CarrierError::Closed) => Err(QuicError::Closed),
            Some(_) => Err(QuicError::Transport),
            None => Ok(()),
        }
    }

    /// The certificates the peer presented in the handshake.
    fn peer_certificates(&self) -> Option<Vec<rustls::pki_types::CertificateDer<'static>>> {
        self.connection
            .crypto_session()
            .peer_identity()?
            .downcast::<Vec<rustls::pki_types::CertificateDer<'static>>>()
            .ok()
            .map(|certificates| *certificates)
    }
}

impl Drop for Driver {
    /// Closes the connection, sends the close, and makes a server forget it at once.
    fn drop(&mut self) {
        let now = Instant::now();
        if !self.connection.is_closed() {
            self.connection
                .close(now, VarInt::from_u32(0), bytes::Bytes::new());
        }
        self.transmit(now);
        if let Side::Server { shared, .. } = &self.side {
            let mut server = shared.lock();
            server
                .endpoint
                .handle_event(self.handle, EndpointEvent::drained());
            server.routes.remove(&self.handle);
        }
    }
}

/// The L0 stream of a QUIC connection before admission: the pairing exchange and the admission
/// preamble, each wait under a deadline. [`QuicStream::into_carrier`] turns it into the
/// connection's carrier.
pub struct QuicStream {
    driver: Driver,
    l0: Option<StreamId>,
    /// L0 bytes read past the last preamble message, which the carrier reads first.
    read: Vec<u8>,
}

impl QuicStream {
    fn new(driver: Driver) -> Self {
        Self {
            driver,
            l0: None,
            read: Vec::new(),
        }
    }

    /// Dials `addresses` in turn until one completes the handshake, then waits for the server's
    /// L0 stream.
    pub fn connect(
        config: ClientConfig,
        addresses: &[SocketAddr],
        deadline: Instant,
    ) -> Result<Self, QuicError> {
        let mut last = QuicError::Transport;
        for (index, address) in addresses.iter().enumerate() {
            // Each address but the last gets at most a second, as a TCP connect would.
            let until = if index + 1 == addresses.len() {
                deadline
            } else {
                deadline.min(Instant::now() + Duration::from_secs(1))
            };
            let local: SocketAddr = if address.is_ipv4() {
                (std::net::Ipv4Addr::UNSPECIFIED, 0).into()
            } else {
                (std::net::Ipv6Addr::UNSPECIFIED, 0).into()
            };
            let socket = nonblocking_socket(local)?;
            if socket.connect(address).is_err() {
                continue;
            }
            let socket = Arc::new(socket);
            let mut endpoint = Box::new(Endpoint::new(
                Arc::new(EndpointConfig::default()),
                None,
                true,
            ));
            let Ok((handle, connection)) =
                endpoint.connect(Instant::now(), config.clone(), *address, SERVER_NAME)
            else {
                continue;
            };
            let driver = Driver {
                connection,
                handle,
                socket,
                peer: Some(*address),
                side: Side::Client {
                    endpoint,
                    buffer: vec![0; DATAGRAM_BYTES],
                },
                state: DriverState::new(),
            };
            let mut stream = Self::new(driver);
            match stream.open_l0(until) {
                Ok(()) => return Ok(stream),
                Err(error) => last = error,
            }
        }
        Err(last)
    }

    /// The server opens L0 once the handshake completes; the client accepts it when its first
    /// bytes arrive.
    fn open_l0(&mut self, deadline: Instant) -> Result<(), QuicError> {
        loop {
            self.driver.pump();
            self.driver.check()?;
            if self.driver.state.connected {
                let opened = match self.driver.connection.side() {
                    noq_proto::Side::Server => self.driver.connection.streams().open(Dir::Bi),
                    noq_proto::Side::Client => self.driver.connection.streams().accept(Dir::Bi),
                };
                if let Some(id) = opened {
                    let _ = self
                        .driver
                        .connection
                        .send_stream(id)
                        .set_priority(level_priority(Priority::L0));
                    self.l0 = Some(id);
                    return Ok(());
                }
            }
            self.driver.wait(deadline)?;
        }
    }

    /// Completes a server connection's handshake and opens its L0 stream.
    pub fn handshake(&mut self, deadline: Instant) -> Result<(), QuicError> {
        self.open_l0(deadline)
    }

    /// Finishes L0 and waits until the peer acknowledged everything on it, so a final answer
    /// arrives before the connection closes.
    pub fn finish(&mut self, deadline: Instant) -> Result<(), QuicError> {
        let id = self.l0()?;
        let _ = self.driver.connection.send_stream(id).finish();
        loop {
            self.driver.pump();
            self.driver.check()?;
            if self.driver.state.finished > 0 {
                return Ok(());
            }
            self.driver.wait(deadline)?;
        }
    }

    /// The certificates the peer presented.
    pub fn peer_certificates(&self) -> Option<Vec<rustls::pki_types::CertificateDer<'static>>> {
        self.driver.peer_certificates()
    }

    fn l0(&self) -> Result<StreamId, QuicError> {
        self.l0.ok_or(QuicError::Transport)
    }

    /// Writes `bytes` on L0 and sends them before `deadline`.
    pub fn send_bytes(&mut self, mut bytes: &[u8], deadline: Instant) -> Result<(), QuicError> {
        let id = self.l0()?;
        loop {
            match self.driver.connection.send_stream(id).write(bytes) {
                Ok(count) => bytes = &bytes[count..],
                Err(noq_proto::WriteError::Blocked) => {}
                Err(_) => return Err(QuicError::Transport),
            }
            self.driver.pump();
            self.driver.check()?;
            if bytes.is_empty() {
                return Ok(());
            }
            self.driver.wait(deadline)?;
        }
    }

    /// Reads exactly `count` L0 bytes before `deadline`.
    fn receive_exact(&mut self, count: usize, deadline: Instant) -> Result<Vec<u8>, QuicError> {
        let id = self.l0()?;
        loop {
            if self.read.len() >= count {
                let rest = self.read.split_off(count);
                return Ok(std::mem::replace(&mut self.read, rest));
            }
            self.driver.pump();
            self.driver.check()?;
            let ended = read_stream(&mut self.driver.connection, id, &mut self.read, count)?;
            if self.read.len() >= count {
                continue;
            }
            if ended {
                return Err(QuicError::Closed);
            }
            self.driver.wait(deadline)?;
        }
    }

    /// One length-prefixed message of at most `max` bytes, without its prefix.
    pub fn receive_prefixed(
        &mut self,
        max: usize,
        deadline: Instant,
    ) -> Result<Vec<u8>, QuicError> {
        let size = self.receive_exact(4, deadline)?;
        let size = u32::from_be_bytes([size[0], size[1], size[2], size[3]]) as usize;
        if size > max {
            return Err(QuicError::Transport);
        }
        self.receive_exact(size, deadline)
    }

    /// Starts the connection's carrier. Bytes read past the preamble stay first on L0.
    pub fn into_carrier(self) -> QuicCarrier {
        QuicCarrier::new(self)
    }
}

/// Reads `id` into `into` while it has data and `into` is under `limit` bytes. Returns whether
/// the stream ended.
fn read_stream(
    connection: &mut Connection,
    id: StreamId,
    into: &mut Vec<u8>,
    limit: usize,
) -> Result<bool, QuicError> {
    let mut stream = connection.recv_stream(id);
    let mut chunks = match stream.read(true) {
        Ok(chunks) => chunks,
        Err(_) => return Err(QuicError::Transport),
    };
    let mut ended = false;
    while into.len() < limit {
        match chunks.next(limit.saturating_sub(into.len()).max(1)) {
            Ok(Some(chunk)) => into.extend_from_slice(&chunk.bytes),
            Ok(None) => {
                ended = true;
                break;
            }
            Err(noq_proto::ReadError::Blocked) => break,
            Err(_) => {
                let _ = chunks.finalize();
                return Err(QuicError::Transport);
            }
        }
    }
    let _ = chunks.finalize();
    Ok(ended)
}

impl crate::protocol::Preamble for QuicStream {
    fn send(&mut self, frame: &[u8], deadline: Instant) -> Result<(), crate::ServerError> {
        self.send_bytes(&crate::protocol::prefixed(frame), deadline)
            .map_err(|_| crate::ServerError::Protocol)
    }

    fn receive(&mut self, max: usize, deadline: Instant) -> Result<Vec<u8>, crate::ServerError> {
        self.receive_prefixed(max, deadline)
            .map_err(|_| crate::ServerError::Protocol)
    }
}

/// Higher sends first when several levels have data.
fn level_priority(priority: Priority) -> i32 {
    match priority {
        Priority::L0 => 2,
        Priority::L1 => 1,
        Priority::L2 => 0,
    }
}

fn index(priority: Priority) -> usize {
    match priority {
        Priority::L0 => 0,
        Priority::L1 => 1,
        Priority::L2 => 2,
    }
}

const LEVELS: [Priority; 3] = [Priority::L0, Priority::L1, Priority::L2];

/// One level's outgoing stream and the accepted frames not yet written to it.
#[derive(Default)]
struct Outgoing {
    id: Option<StreamId>,
    frames: VecDeque<Vec<u8>>,
    /// Bytes of the first frame already written.
    offset: usize,
    finished: bool,
}

/// One incoming stream: L0, or a peer's L1 or L2 once its level byte arrived.
struct Incoming {
    id: StreamId,
    /// The level, once known.
    level: Option<usize>,
    /// Whether the level byte is still to be read.
    unlabelled: bool,
    reader: FrameReader,
    ended: bool,
}

/// Stops a QUIC carrier from receiving, for revocation. Writing continues, so a queued goodbye
/// still leaves.
pub struct QuicSever {
    severed: Arc<std::sync::atomic::AtomicBool>,
    ring: Option<UnixStream>,
}

impl super::registry::Sever for QuicSever {
    fn sever(&self) {
        self.severed
            .store(true, std::sync::atomic::Ordering::Release);
        if let Some(bell) = &self.ring {
            ring(bell);
        }
    }
}

pub struct QuicCarrier {
    driver: Driver,
    outgoing: [Outgoing; 3],
    incoming: Vec<Incoming>,
    inbound: VecDeque<RawFrame>,
    inbound_bytes: usize,
    /// Peer streams of other levels this end accepted, so a third is a violation.
    labelled: [bool; 3],
    severed: Arc<std::sync::atomic::AtomicBool>,
    /// Whether this end is the client, which sends every level on L0.
    client: bool,
    /// Set to move a client to a new socket at its next call.
    rebind: Arc<std::sync::atomic::AtomicBool>,
    /// The server's goodbye, held until its L1 and L2 streams ended.
    goodbye: Option<RawFrame>,
    /// Packets sent, so a flush reports progress.
    sent: u64,
    closing: bool,
    violation: Option<CarrierError>,
}

impl QuicCarrier {
    fn new(stream: QuicStream) -> Self {
        let QuicStream { driver, l0, read } = stream;
        let mut outgoing: [Outgoing; 3] = Default::default();
        outgoing[0].id = l0;
        let mut reader = FrameReader::default();
        reader.feed(&read);
        let incoming = l0
            .map(|id| Incoming {
                id,
                level: Some(0),
                unlabelled: false,
                reader,
                ended: false,
            })
            .into_iter()
            .collect();
        let client = matches!(driver.side, Side::Client { .. });
        Self {
            client,
            rebind: Arc::default(),
            goodbye: None,
            driver,
            outgoing,
            incoming,
            inbound: VecDeque::new(),
            inbound_bytes: 0,
            labelled: [true, false, false],
            severed: Arc::default(),
            sent: 0,
            closing: false,
            violation: None,
        }
    }

    /// A handle that severs this connection's receiving. A server connection's bell wakes its
    /// thread.
    pub fn sever(&self) -> QuicSever {
        let ring = match &self.driver.side {
            Side::Server { route, .. } => route.ring.try_clone().ok(),
            Side::Client { .. } => None,
        };
        QuicSever {
            severed: Arc::clone(&self.severed),
            ring,
        }
    }

    /// A handle that moves this client to a new UDP socket at its next turn, as a network change
    /// does. Ring the link after setting it.
    pub fn rebinder(&self) -> Arc<std::sync::atomic::AtomicBool> {
        Arc::clone(&self.rebind)
    }

    fn rebind_if_asked(&mut self) {
        if self.rebind.swap(false, std::sync::atomic::Ordering::AcqRel) {
            self.driver.rebind(Instant::now());
        }
    }

    fn packets_sent(&mut self) -> u64 {
        self.driver.connection.stats().udp_tx.datagrams
    }

    /// Writes accepted frames into their streams until QUIC's buffers are full.
    fn write_streams(&mut self) -> Result<(), CarrierError> {
        for level in LEVELS {
            let outgoing = &mut self.outgoing[index(level)];
            while let Some(front) = outgoing.frames.front() {
                let id = match outgoing.id {
                    Some(id) => id,
                    None => {
                        let Some(id) = self.driver.connection.streams().open(Dir::Uni) else {
                            break;
                        };
                        let mut stream = self.driver.connection.send_stream(id);
                        let _ = stream.set_priority(level_priority(level));
                        if stream.write(&[index(level) as u8]) != Ok(1) {
                            return Err(CarrierError::Failed);
                        }
                        outgoing.id = Some(id);
                        id
                    }
                };
                match self
                    .driver
                    .connection
                    .send_stream(id)
                    .write(&front[outgoing.offset..])
                {
                    Ok(count) => {
                        outgoing.offset += count;
                        if outgoing.offset == front.len() {
                            outgoing.frames.pop_front();
                            outgoing.offset = 0;
                        }
                    }
                    Err(noq_proto::WriteError::Blocked) => break,
                    Err(_) => return Err(CarrierError::Failed),
                }
            }
            if self.closing && outgoing.frames.is_empty() && !outgoing.finished {
                // The server finishes every level, opening any it never used, so the client knows
                // when nothing is left in flight before the goodbye.
                if outgoing.id.is_none() && !self.client {
                    let Some(id) = self.driver.connection.streams().open(Dir::Uni) else {
                        continue;
                    };
                    if self
                        .driver
                        .connection
                        .send_stream(id)
                        .write(&[index(level) as u8])
                        != Ok(1)
                    {
                        return Err(CarrierError::Failed);
                    }
                    outgoing.id = Some(id);
                }
                outgoing.finished = true;
                if let Some(id) = outgoing.id {
                    let _ = self.driver.connection.send_stream(id).finish();
                }
            }
        }
        Ok(())
    }

    /// Whether the server's L1 and L2 streams both ended, so nothing sent before its goodbye is
    /// still in flight.
    fn levels_ended(&self) -> bool {
        [1, 2].iter().all(|level| {
            self.incoming
                .iter()
                .any(|stream| stream.level == Some(*level) && stream.ended)
        })
    }

    fn unflushed(&self) -> usize {
        self.outgoing.iter().map(|level| level.frames.len()).sum()
    }

    /// Accepts the peer's L1 and L2 streams and reads every stream until the inbound bound.
    fn read_streams(&mut self) -> Result<(), CarrierError> {
        while let Some(id) = self.driver.connection.streams().accept(Dir::Uni) {
            self.incoming.push(Incoming {
                id,
                level: None,
                unlabelled: true,
                reader: FrameReader::default(),
                ended: false,
            });
        }
        let mut bytes = Vec::new();
        for stream in &mut self.incoming {
            while !stream.ended && self.inbound_bytes < INBOUND_BYTES {
                bytes.clear();
                let ended = read_stream(
                    &mut self.driver.connection,
                    stream.id,
                    &mut bytes,
                    READ_CHUNK,
                )
                .map_err(|_| CarrierError::Failed)?;
                stream.ended = ended;
                let mut data = bytes.as_slice();
                if stream.unlabelled
                    && let Some((&level, rest)) = data.split_first()
                {
                    let level = usize::from(level);
                    if !matches!(level, 1 | 2) || self.labelled[level] {
                        return Err(CarrierError::Decode(
                            crate::protocol::DecodeError::Malformed,
                        ));
                    }
                    self.labelled[level] = true;
                    stream.level = Some(level);
                    stream.unlabelled = false;
                    data = rest;
                }
                stream.reader.feed(data);
                while let Some(frame) = stream.reader.next_frame()? {
                    if self.client && frame.kind == crate::protocol::Kind::Goodbye as u8 {
                        self.goodbye.get_or_insert(frame);
                        continue;
                    }
                    self.inbound_bytes += limits::HEADER_BYTES + frame.body.len();
                    self.inbound.push_back(frame);
                }
                if ended && stream.reader.partial() {
                    return Err(CarrierError::Decode(
                        crate::protocol::DecodeError::Malformed,
                    ));
                }
                if bytes.is_empty() {
                    break;
                }
            }
        }
        Ok(())
    }
}

/// Stream bytes read at once.
const READ_CHUNK: usize = 64 * 1024;

impl Carrier for QuicCarrier {
    fn offer(&mut self, frame: &[u8], priority: Priority) -> Result<Offer, CarrierError> {
        if self.closing {
            return Err(CarrierError::Closed);
        }
        if let Some(error) = self.driver.state.lost {
            return Err(error);
        }
        if self.unflushed() > 1 {
            return Ok(Offer::Full);
        }
        let level = if self.client { Priority::L0 } else { priority };
        self.outgoing[index(level)].frames.push_back(frame.to_vec());
        Ok(Offer::Accepted)
    }

    fn flush(&mut self) -> Result<Flush, CarrierError> {
        self.rebind_if_asked();
        // Frames first, so acknowledgements of what `receive` took ride on them.
        self.write_streams()?;
        if self.closing {
            // Nothing receives any more, so acknowledgements are taken here.
            self.driver.pump();
            self.write_streams()?;
        }
        self.driver.advance(Instant::now());
        let sent = self.packets_sent();
        let progressed = sent != self.sent;
        self.sent = sent;
        let opened = self
            .outgoing
            .iter()
            .filter(|level| level.id.is_some())
            .count();
        let pending = if self.closing {
            self.unflushed() > 0 || self.driver.state.finished < opened
        } else {
            self.unflushed() > 0
        } || self.driver.state.stashed.is_some();
        if pending && let Some(error) = self.driver.state.lost {
            return Err(error);
        }
        Ok(Flush {
            progressed,
            pending,
        })
    }

    fn receive(&mut self) -> Result<Option<RawFrame>, CarrierError> {
        if self.closing || self.severed.load(std::sync::atomic::Ordering::Acquire) {
            return Err(CarrierError::Closed);
        }
        if let Some(error) = self.violation {
            return Err(error);
        }
        if self.inbound.is_empty() {
            // Datagrams only: acknowledgements, timers and flow-control updates wait for the
            // `flush` that follows every turn, so a frame reaches the caller before any send.
            self.driver.ingest(Instant::now());
            if let Err(error) = self.read_streams() {
                self.violation = Some(error);
            }
        }
        if let Some(frame) = self.inbound.pop_front() {
            self.inbound_bytes -= limits::HEADER_BYTES + frame.body.len();
            return Ok(Some(frame));
        }
        if self.goodbye.is_some() && self.levels_ended() {
            return Ok(self.goodbye.take());
        }
        if let Some(error) = self.violation.or(self.driver.state.lost) {
            return Err(error);
        }
        Ok(None)
    }

    fn readiness(&self) -> Readiness {
        let descriptor = self.driver.descriptor();
        Readiness {
            read: descriptor,
            write: self
                .driver
                .state
                .stashed
                .is_some()
                .then(|| self.driver.socket.as_raw_fd()),
            // Acknowledgements arrive on the same descriptor and free QUIC's buffers.
            progress: Some(descriptor),
            deadline: self.driver.connection.poll_timeout(),
        }
    }

    fn close(&mut self) {
        self.closing = true;
    }
}
