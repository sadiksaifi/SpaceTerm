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
//!   descriptor is the progress descriptor. A client also reports its route-change signal and the
//!   next check of its preferred local address, from [`super::path`].
use super::carrier::{Carrier, CarrierError, Flush, Offer, Priority, Readiness};
use super::path::PathWatch;
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
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock, RwLockReadGuard};
use std::time::{Duration, Instant};

/// The protocol's own ALPN.
pub const ALPN: &[u8] = b"spaceterm/2";
/// Received frame bytes that `receive` has not taken yet. Above it the carrier stops reading
/// streams, so the peer's sending waits on QUIC flow control.
const INBOUND_BYTES: usize = 2 * limits::MAX_FRAME_BYTES;
/// Largest UDP payload either side accepts: noq's default, an Ethernet MTU without the IP and UDP
/// headers. Every endpoint advertises it, so a longer datagram is not QUIC's and is cut short.
const DATAGRAM_BYTES: usize = 1472;
/// Datagrams and bytes routed to one server connection that its thread has not taken yet. Above
/// either, the listener drops datagrams as a full socket buffer would; QUIC retransmits.
const ROUTED_DATAGRAMS: usize = 256;
const ROUTED_BYTES: usize = ROUTED_DATAGRAMS * DATAGRAM_BYTES;
/// Each side's flow-control credit for the whole connection until admission: the pairing and
/// admission messages and nothing more, so an unauthenticated peer can make the other buffer
/// little.
const ADMISSION_WINDOW: u32 = 64 * 1024;
/// The connection's credit after admission: one largest frame on each of two streams.
const ADMITTED_WINDOW: u32 = 2 * limits::MAX_FRAME_BYTES as u32;
/// Connections whose handshake started and that the host has not taken yet.
const MAX_PENDING: usize = 32;
/// How long a client tries each address but the last. QUIC's first probe timeout is about one
/// second at its initial 333 ms round-trip estimate, so one lost Initial packet or Retry is sent
/// again, and its answer has another second to arrive, before the client moves on.
const ADDRESS_LIMIT: Duration = Duration::from_secs(2);
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
    #[error("the TLS handshake was refused")]
    Refused,
    #[error("the peer speaks another protocol")]
    Incompatible,
    #[error("the address is in use")]
    AddressInUse,
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
        .keep_alive_interval(Some(Duration::from_secs(10)))
        // No unreliable datagrams. A stream holds at most one largest frame unread, and the
        // connection only what admission needs until the carrier raises it.
        .datagram_receive_buffer_size(None)
        .stream_receive_window(VarInt::from_u32(limits::MAX_FRAME_BYTES as u32))
        .receive_window(VarInt::from_u32(ADMISSION_WINDOW));
    Arc::new(config)
}

fn endpoint_config() -> Arc<EndpointConfig> {
    let mut config = EndpointConfig::default();
    // Matches what the listener and clients read at once.
    let _ = config.max_udp_payload_size(DATAGRAM_BYTES as u16);
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

/// A nonblocking UDP socket with noq-udp's options: the destination address of each received
/// datagram, so a server answers from the address the client dialed, and don't-fragment, so path
/// MTU discovery never passes a probe in fragments.
struct Udp {
    socket: UdpSocket,
    state: noq_udp::UdpSocketState,
    ipv4: bool,
    /// The server a client's socket is connected to.
    connected: Option<SocketAddr>,
}

impl Udp {
    /// Binds `address`. An IPv6 socket takes only IPv6, so the IPv4 wildcard of the same port
    /// stays free for a socket of its own.
    fn bind(address: SocketAddr) -> Result<Self, QuicError> {
        let domain = socket2::Domain::for_address(address);
        let socket = socket2::Socket::new(domain, socket2::Type::DGRAM, None)
            .map_err(|_| QuicError::Transport)?;
        if address.is_ipv6() {
            socket.set_only_v6(true).map_err(|_| QuicError::Transport)?;
        }
        socket.bind(&address.into()).map_err(|error| {
            if error.kind() == ErrorKind::AddrInUse {
                QuicError::AddressInUse
            } else {
                QuicError::Transport
            }
        })?;
        let socket = UdpSocket::from(socket);
        socket
            .set_nonblocking(true)
            .map_err(|_| QuicError::Transport)?;
        let state =
            noq_udp::UdpSocketState::new((&socket).into()).map_err(|_| QuicError::Transport)?;
        Ok(Self {
            socket,
            state,
            ipv4: address.is_ipv4(),
            connected: None,
        })
    }

    /// A client's socket, bound by the system and connected to `peer`, which keeps the kernel's
    /// route lookup off every send.
    fn client(peer: SocketAddr) -> Result<Self, QuicError> {
        let local: SocketAddr = if peer.is_ipv4() {
            (std::net::Ipv4Addr::UNSPECIFIED, 0).into()
        } else {
            (std::net::Ipv6Addr::UNSPECIFIED, 0).into()
        };
        let mut udp = Self::bind(local)?;
        udp.socket.connect(peer).map_err(|_| QuicError::Transport)?;
        udp.connected = Some(peer);
        Ok(udp)
    }

    /// Sends one datagram from `source`, when known. macOS refuses an explicit address on a
    /// connected socket, so a client sends plainly.
    fn send(
        &self,
        bytes: &[u8],
        destination: SocketAddr,
        source: Option<std::net::IpAddr>,
    ) -> std::io::Result<()> {
        if self.connected == Some(destination) {
            return self.socket.send(bytes).map(drop);
        }
        #[cfg(target_vendor = "apple")]
        if let (Some(std::net::IpAddr::V4(source)), SocketAddr::V4(destination)) =
            (source, destination)
        {
            return send_from(&self.socket, bytes, destination, source);
        }
        self.state.try_send(
            (&self.socket).into(),
            &noq_udp::Transmit {
                destination,
                ecn: None,
                contents: bytes,
                segment_size: None,
                src_ip: source,
            },
        )
    }

    /// Reads one batch of datagrams into `buffer` and hands each to `each` with its sender and
    /// the local address it reached. Datagrams the kernel coalesced are split again; one longer
    /// than [`DATAGRAM_BYTES`] is not QUIC's and is dropped.
    fn receive(
        &self,
        buffer: &mut [u8],
        mut each: impl FnMut(SocketAddr, Option<std::net::IpAddr>, &[u8]),
    ) -> std::io::Result<()> {
        let mut meta = [noq_udp::RecvMeta::default()];
        let count = self.state.recv(
            (&self.socket).into(),
            &mut [std::io::IoSliceMut::new(buffer)],
            &mut meta,
        )?;
        for meta in &meta[..count] {
            let stride = meta.stride.max(1);
            for datagram in buffer[..meta.len].chunks(stride) {
                if datagram.len() <= DATAGRAM_BYTES {
                    each(meta.addr, meta.dst_ip, datagram);
                }
            }
        }
        Ok(())
    }

    fn fd(&self) -> RawFd {
        self.socket.as_raw_fd()
    }

    fn port(&self) -> Option<u16> {
        self.socket.local_addr().ok().map(|address| address.port())
    }
}

/// Sends one IPv4 datagram from `source`. macOS ignores the `IP_RECVDSTADDR` source that noq-udp
/// encodes and honors only `IP_PKTINFO`, so without this a socket on every interface would answer
/// from whichever address the system routes from, and the client would drop the answer.
#[cfg(target_vendor = "apple")]
fn send_from(
    socket: &UdpSocket,
    bytes: &[u8],
    destination: std::net::SocketAddrV4,
    source: std::net::Ipv4Addr,
) -> std::io::Result<()> {
    let address = socket2::SockAddr::from(destination);
    let mut iovec = libc::iovec {
        iov_base: bytes.as_ptr().cast_mut().cast(),
        iov_len: bytes.len(),
    };
    let info = libc::in_pktinfo {
        ipi_ifindex: 0,
        ipi_spec_dst: libc::in_addr {
            s_addr: u32::from_ne_bytes(source.octets()),
        },
        ipi_addr: libc::in_addr { s_addr: 0 },
    };
    let info_bytes = std::mem::size_of::<libc::in_pktinfo>() as libc::c_uint;
    // Aligned for `cmsghdr`, and larger than one header with its `in_pktinfo`.
    let mut control = [0_u64; 8];
    // SAFETY: an all-zero `msghdr` is valid; every pointer set below outlives the call.
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_name = address.as_ptr().cast_mut().cast();
    message.msg_namelen = address.len();
    message.msg_iov = &mut iovec;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    // SAFETY: CMSG_SPACE only computes a length.
    message.msg_controllen = unsafe { libc::CMSG_SPACE(info_bytes) } as _;
    // SAFETY: the control buffer holds one header and its data, so CMSG_FIRSTHDR points inside
    // it and CMSG_DATA has room for `in_pktinfo`, written unaligned.
    unsafe {
        let header = libc::CMSG_FIRSTHDR(&message);
        (*header).cmsg_level = libc::IPPROTO_IP;
        (*header).cmsg_type = libc::IP_PKTINFO;
        (*header).cmsg_len = libc::CMSG_LEN(info_bytes) as _;
        std::ptr::write_unaligned(libc::CMSG_DATA(header).cast::<libc::in_pktinfo>(), info);
    }
    loop {
        // SAFETY: `message` describes live buffers for the whole call.
        if unsafe { libc::sendmsg(socket.as_raw_fd(), &message, 0) } >= 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// A listener's sockets: one at its address, or one per family for every interface. Each reply
/// leaves from the address its client dialed, which packet info reports on receipt.
struct Sockets {
    address: SocketAddr,
    udp: Vec<Udp>,
}

impl Sockets {
    /// Binds `address`. The unspecified address of either family binds `0.0.0.0` and `[::]` at
    /// the same port, the IPv6 one only where the system offers IPv6.
    fn bind(address: SocketAddr) -> Result<Self, QuicError> {
        if !address.ip().is_unspecified() {
            let udp = Udp::bind(address)?;
            let port = udp.port().ok_or(QuicError::Transport)?;
            return Ok(Self {
                address: SocketAddr::new(address.ip(), port),
                udp: vec![udp],
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
        })
    }

    /// None, after a failed rebind could not restore the previous address.
    fn none(address: SocketAddr) -> Self {
        Self {
            address,
            udp: Vec::new(),
        }
    }

    fn for_destination(&self, destination: SocketAddr) -> Option<&Udp> {
        self.udp
            .iter()
            .find(|udp| udp.ipv4 == destination.is_ipv4())
    }
}

/// Bytes read from a socket at once: one datagram, or several the kernel coalesced.
const RECEIVE_BYTES: usize = 64 * 1024;

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
    events: Mutex<Bounded<ConnectionEvent>>,
    ring: UnixStream,
}

/// A queue that refuses items past a count and a byte total.
struct Bounded<T> {
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
    fn push(&mut self, item: T, bytes: usize) -> bool {
        if self.items.len() >= ROUTED_DATAGRAMS || self.bytes + bytes > ROUTED_BYTES {
            return false;
        }
        self.items.push_back(item);
        self.bytes += bytes;
        true
    }

    fn take(&mut self) -> VecDeque<T> {
        self.bytes = 0;
        std::mem::take(&mut self.items)
    }
}

struct Shared {
    server: Mutex<Server>,
    /// Replaced only when the listener widens to every interface.
    sockets: RwLock<Sockets>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Server> {
        self.server.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn sockets(&self) -> RwLockReadGuard<'_, Sockets> {
        self.sockets.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Sends one datagram from the socket of `destination`'s family.
    fn send(
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

    /// Replaces the sockets with ones at `address`, or binds the previous address again when
    /// that fails.
    fn rebind(&self, address: SocketAddr) -> Result<(), QuicError> {
        let mut sockets = self.sockets.write().unwrap_or_else(|e| e.into_inner());
        let previous = sockets.address;
        *sockets = Sockets::none(previous);
        match Sockets::bind(address) {
            Ok(bound) => {
                *sockets = bound;
                Ok(())
            }
            Err(error) => {
                if let Ok(restored) = Sockets::bind(previous) {
                    *sockets = restored;
                }
                Err(error)
            }
        }
    }

    /// The descriptor that turns writable when the socket of `destination`'s family takes
    /// datagrams again.
    fn descriptor(&self, destination: SocketAddr) -> Option<RawFd> {
        self.sockets().for_destination(destination).map(Udp::fd)
    }
}

/// The server's UDP sockets and endpoint. A thread of its own reads every datagram and routes it
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
    /// [`ACCEPTING`], [`RETIRED`] or [`FINISHED`]. One atomic, so a revive and the thread's
    /// finish never both win.
    phase: AtomicU8,
    /// Set by `Drop`: the thread ends at once, even with live connections.
    stopping: AtomicBool,
    rebinding: Mutex<Rebinding>,
}

/// A rebind the listener's thread performs at its next turn.
enum Rebinding {
    None,
    /// To this address, answering on the sender.
    Asked(
        SocketAddr,
        std::sync::mpsc::SyncSender<Result<(), QuicError>>,
    ),
    /// The thread ended, so nothing rebinds.
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
        let demux = Arc::new(Demux {
            shared: Arc::new(Shared {
                server: Mutex::new(Server {
                    endpoint,
                    routes: HashMap::new(),
                }),
                sockets: RwLock::new(sockets),
            }),
            config,
            accepted: Mutex::default(),
            wake: Mutex::default(),
            phase: AtomicU8::new(ACCEPTING),
            stopping: AtomicBool::new(false),
            rebinding: Mutex::new(Rebinding::None),
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

    /// The address and port the listener is bound to. Every interface reads as `0.0.0.0`.
    pub fn address(&self) -> SocketAddr {
        self.demux.shared.sockets().address
    }

    /// Moves the listener to `address` and keeps its connections, as when exposure widens from
    /// one address to every interface at the same port. An address and the wildcard of its port
    /// can't both be bound, so the old socket closes just before the new one binds; a datagram
    /// arriving in between is lost and QUIC sends it again. When `address` can't be bound the
    /// listener binds its old address again.
    pub fn rebind(&self, address: SocketAddr) -> Result<(), QuicError> {
        let (done, result) = std::sync::mpsc::sync_channel(1);
        {
            let mut rebinding = self
                .demux
                .rebinding
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if matches!(*rebinding, Rebinding::Ended) {
                return Err(QuicError::Transport);
            }
            *rebinding = Rebinding::Asked(address, done);
        }
        // Linux keeps a socket open while a poll waits on it, so the thread closes the old socket
        // between its waits.
        ring(&self.stop);
        result.recv().unwrap_or(Err(QuicError::Transport))
    }

    /// Stops taking connections but keeps routing to the live ones until the last ends, so
    /// their goodbyes still leave and their acknowledgements still arrive. Connections that
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
        ring(&self.stop);
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
        ring(&self.stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Demux {
    fn run(&self, stopped: &UnixStream) {
        self.serve(stopped);
        // Drops a rebind asked meanwhile, which then fails.
        *self.rebinding.lock().unwrap_or_else(|e| e.into_inner()) = Rebinding::Ended;
    }

    /// Waits on the sockets and the stop bell, rebinds when asked, and routes every datagram.
    fn serve(&self, stopped: &UnixStream) {
        let mut buffer = vec![0; RECEIVE_BYTES];
        let mut send = Vec::new();
        let mut descriptors = Vec::with_capacity(3);
        loop {
            let poll = |fd| libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            descriptors.clear();
            descriptors.push(poll(stopped.as_raw_fd()));
            descriptors.extend(self.shared.sockets().udp.iter().map(|udp| poll(udp.fd())));
            // A retired listener looks for its last connection's end now and then.
            let retired = self.phase.load(Ordering::Acquire) == RETIRED;
            let timeout = if retired { 100 } else { -1 };
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
                &mut *self.rebinding.lock().unwrap_or_else(|e| e.into_inner()),
                Rebinding::None,
            );
            if let Rebinding::Asked(address, done) = asked {
                let _ = done.send(self.shared.rebind(address));
            }
            self.route(&mut buffer, &mut send);
            if self.try_finish() {
                // The host binds again once a retired socket is free.
                if let Some(wake) = &*self.wake.lock().unwrap_or_else(|e| e.into_inner()) {
                    wake.wake();
                }
                return;
            }
        }
    }

    /// Finishes a retired listener whose last connection ended. A revive that came first keeps
    /// it running; one that comes later finds it finished.
    fn try_finish(&self) -> bool {
        let server = self.shared.lock();
        server.routes.is_empty()
            && self
                .phase
                .compare_exchange(RETIRED, FINISHED, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
    }

    fn send(&self, bytes: &[u8], destination: SocketAddr, source: Option<std::net::IpAddr>) {
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
                if let Some(route) = server.routes.get(&handle)
                    && route
                        .events
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push(event, count)
                {
                    ring(&route.ring);
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
                                connection,
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

/// Where a connection's datagrams come from.
enum Side {
    /// A client owns its socket and endpoint.
    Client {
        socket: Udp,
        endpoint: Box<Endpoint>,
        buffer: Vec<u8>,
    },
    /// A server connection receives what the listener routes to it, and rings its bell. It sends
    /// on the listener's sockets.
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
    /// The TLS alert the connection ended with, if any.
    alert: Option<u8>,
    /// Send streams whose data the peer acknowledged after this end finished them.
    finished: usize,
    /// A datagram that would have blocked, retried before any other.
    stashed: Option<(SocketAddr, Option<std::net::IpAddr>, Vec<u8>)>,
    transmit: Vec<u8>,
}

impl DriverState {
    fn new() -> Self {
        Self {
            connected: false,
            lost: None,
            alert: None,
            finished: 0,
            stashed: None,
            transmit: Vec::new(),
        }
    }
}

/// One QUIC connection and its I/O, driven only when its thread calls it.
struct Driver {
    connection: Connection,
    handle: ConnectionHandle,
    /// The server a client's socket is connected to.
    peer: Option<SocketAddr>,
    side: Side,
    state: DriverState,
}

impl Driver {
    /// The descriptor that becomes readable when datagrams arrive for this connection.
    fn descriptor(&self) -> RawFd {
        match &self.side {
            Side::Client { socket, .. } => socket.fd(),
            Side::Server { bell, .. } => bell.as_raw_fd(),
        }
    }

    fn send(
        &self,
        bytes: &[u8],
        destination: SocketAddr,
        source: Option<std::net::IpAddr>,
    ) -> std::io::Result<()> {
        match &self.side {
            Side::Client { socket, .. } => socket.send(bytes, destination, source),
            Side::Server { shared, .. } => shared.send(bytes, destination, source),
        }
    }

    /// The descriptor that turns writable once a stashed datagram can be sent.
    fn stashed_descriptor(&self) -> Option<RawFd> {
        let (destination, ..) = self.state.stashed.as_ref()?;
        match &self.side {
            Side::Client { socket, .. } => Some(socket.fd()),
            Side::Server { shared, .. } => shared.descriptor(*destination),
        }
    }

    /// Takes arrived datagrams, runs expired timers, exchanges endpoint events, records the
    /// connection's events and sends what it has.
    fn pump(&mut self) {
        let now = Instant::now();
        let _ = self.ingest(now);
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
                    // TLS alerts travel as QUIC crypto errors, 0x100 to 0x1ff.
                    let alert = |code: noq_proto::TransportErrorCode| {
                        let code = u64::from(code);
                        (0x100..0x200)
                            .contains(&code)
                            .then_some((code & 0xff) as u8)
                    };
                    let closed = match &reason {
                        ConnectionError::ConnectionClosed(close) => alert(close.error_code),
                        ConnectionError::TransportError(error) => alert(error.code),
                        _ => None,
                    };
                    self.state.alert = self.state.alert.or(closed);
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

    /// Takes arrived datagrams. Returns whether any arrived.
    fn ingest(&mut self, now: Instant) -> bool {
        let arrived;
        match &mut self.side {
            Side::Client {
                socket,
                endpoint,
                buffer,
            } => {
                let mut datagrams = Vec::new();
                loop {
                    let received = socket.receive(buffer, |remote, local, bytes| {
                        datagrams.push((remote, local, BytesMut::from(bytes)));
                    });
                    match received {
                        Ok(()) => {}
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
                    }
                }
                arrived = !datagrams.is_empty();
                for (remote, local, data) in datagrams {
                    // noq-proto reads a close's reason only when its events are polled, and a
                    // later stateless reset replaces it. A server forgets a connection as soon
                    // as it closes, so its reset to this end's next packet can arrive in the same
                    // batch as its close, and the TLS alert of a refusal would be lost.
                    if self.connection.is_closed() {
                        break;
                    }
                    self.state.transmit.clear();
                    match endpoint.handle(
                        now,
                        FourTuple::new(remote, local),
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
                            let _ = socket.send(
                                &self.state.transmit[..transmit.size],
                                transmit.destination,
                                transmit.src_ip,
                            );
                        }
                        _ => {}
                    }
                }
            }
            Side::Server { route, bell, .. } => {
                drain_bell(bell);
                let events = route
                    .events
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take();
                arrived = !events.is_empty();
                for event in events {
                    self.connection.handle_event(event);
                }
            }
        }
        arrived
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
        if let Some((destination, source, bytes)) = self.state.stashed.take() {
            match self.send(&bytes, destination, source) {
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    self.state.stashed = Some((destination, source, bytes));
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
            match self.send(bytes, transmit.destination, transmit.src_ip) {
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    self.state.stashed =
                        Some((transmit.destination, transmit.src_ip, bytes.to_vec()));
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
                    let _ = self.rebind(now);
                    return;
                }
                // A lost datagram is QUIC's to recover.
                _ => {}
            }
        }
    }

    /// Moves a client to a new UDP socket, as after a switch between networks. The server
    /// validates the new path and the connection continues without a new handshake. Returns
    /// whether the client moved.
    fn rebind(&mut self, now: Instant) -> bool {
        let Some(peer) = self.peer else {
            return false;
        };
        let Side::Client { socket, .. } = &mut self.side else {
            return false;
        };
        let Ok(moved) = Udp::client(peer) else {
            return false;
        };
        *socket = moved;
        self.state.stashed = None;
        self.connection.handle_network_change(None, now);
        self.transmit(now);
        true
    }

    /// The local address a client's socket uses.
    fn local_ip(&self) -> Option<std::net::IpAddr> {
        self.local_address().map(|address| address.ip())
    }

    /// The local address and port of a client's socket.
    fn local_address(&self) -> Option<SocketAddr> {
        match &self.side {
            Side::Client { socket, .. } => socket.socket.local_addr().ok(),
            Side::Server { .. } => None,
        }
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
        let mut descriptors = vec![libc::pollfd {
            fd: self.descriptor(),
            events: libc::POLLIN,
            revents: 0,
        }];
        // A stashed datagram waits for the socket to take it, not for the bell.
        if let Some(fd) = self.stashed_descriptor() {
            descriptors.push(libc::pollfd {
                fd,
                events: libc::POLLOUT,
                revents: 0,
            });
        }
        let remaining = until.saturating_duration_since(Instant::now());
        let timeout = remaining.as_micros().div_ceil(1000).min(i32::MAX as u128) as i32;
        // SAFETY: `descriptors` is a live, writable array of `pollfd` for the whole call.
        unsafe {
            libc::poll(
                descriptors.as_mut_ptr(),
                descriptors.len() as libc::nfds_t,
                timeout,
            )
        };
        Ok(())
    }

    fn check(&self) -> Result<(), QuicError> {
        // A client counts its handshake done before the server judged its certificate.
        match self.state.alert {
            // no_application_protocol: another QUIC service, or another major's ALPN.
            Some(120) => return Err(QuicError::Incompatible),
            Some(_) => return Err(QuicError::Refused),
            None => {}
        }
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
        server_verified: &AtomicBool,
    ) -> Result<Self, QuicError> {
        let mut last = QuicError::Transport;
        for (index, address) in addresses.iter().enumerate() {
            server_verified.store(false, Ordering::Release);
            let until = if index + 1 == addresses.len() {
                deadline
            } else {
                deadline.min(Instant::now() + ADDRESS_LIMIT)
            };
            let Ok(socket) = Udp::client(*address) else {
                continue;
            };
            let mut endpoint = Box::new(Endpoint::new(endpoint_config(), None, true));
            let Ok((handle, connection)) =
                endpoint.connect(Instant::now(), config.clone(), *address, SERVER_NAME)
            else {
                continue;
            };
            let driver = Driver {
                connection,
                handle,
                peer: Some(*address),
                side: Side::Client {
                    socket,
                    endpoint,
                    buffer: vec![0; RECEIVE_BYTES],
                },
                state: DriverState::new(),
            };
            let mut stream = Self::new(driver);
            match stream.open_l0(until) {
                Ok(()) => return Ok(stream),
                // A refusal counts only once this end verified the server: before that, the
                // close may come from another service or a forged packet, so try the next address.
                Err(QuicError::Refused) if server_verified.load(Ordering::Acquire) => {
                    return Err(QuicError::Refused);
                }
                Err(QuicError::Refused) => last = QuicError::Transport,
                Err(error) => last = error,
            }
        }
        Err(last)
    }

    /// The server opens L0 once the handshake completes; the client accepts it when its first
    /// bytes arrive. Those bytes can arrive in the same read as the server's close, so L0 is
    /// taken before the close is reported, and the bytes are still read.
    fn open_l0(&mut self, deadline: Instant) -> Result<(), QuicError> {
        loop {
            self.driver.pump();
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
            self.driver.check()?;
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

    /// Reads exactly `count` L0 bytes before `deadline`. Bytes that arrived before the peer
    /// closed are still read: a peer's final answer is acknowledged as it arrives, so the peer
    /// may close before this end reads it.
    fn receive_exact(&mut self, count: usize, deadline: Instant) -> Result<Vec<u8>, QuicError> {
        let id = self.l0()?;
        loop {
            if self.read.len() >= count {
                let rest = self.read.split_off(count);
                return Ok(std::mem::replace(&mut self.read, rest));
            }
            self.driver.pump();
            let ended = match read_stream(&mut self.driver.connection, id, &mut self.read, count) {
                Ok(ended) => ended,
                Err(error) => {
                    self.driver.check()?;
                    return Err(error);
                }
            };
            if self.read.len() >= count {
                continue;
            }
            self.driver.check()?;
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
    /// A client's watch on the local address the system prefers for its server.
    path: Option<PathWatch>,
    /// The server's goodbye, held until its L1 and L2 streams ended.
    goodbye: Option<RawFrame>,
    /// Packets sent, so a flush reports progress.
    sent: u64,
    closing: bool,
    violation: Option<CarrierError>,
}

impl QuicCarrier {
    fn new(stream: QuicStream) -> Self {
        let QuicStream {
            mut driver,
            l0,
            read,
        } = stream;
        // Admission proved the peer, so the connection may now hold frames.
        driver
            .connection
            .set_receive_window(VarInt::from_u32(ADMITTED_WINDOW));
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
            path: client.then(|| PathWatch::new(Instant::now())),
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

    /// Moves a client to a new socket when its rebinder asked, or when the system now prefers
    /// another local address for its server.
    fn rebind_if_needed(&mut self) {
        let now = Instant::now();
        if self.rebind.swap(false, std::sync::atomic::Ordering::AcqRel) {
            let _ = self.driver.rebind(now);
        }
        if let (Some(path), Some(peer)) = (self.path.as_mut(), self.driver.peer) {
            let current = self.driver.local_ip();
            path.follow(now, peer, current, || self.driver.rebind(now));
        }
    }

    /// Replaces how this client learns its preferred local address, and checks at its next call.
    #[cfg(test)]
    pub(crate) fn prefer_sources(
        &mut self,
        source: impl Fn(SocketAddr) -> Option<std::net::IpAddr> + Send + 'static,
    ) {
        if let Some(path) = self.path.as_mut() {
            path.set_source(Box::new(source));
        }
    }

    /// The local address and port of a client's socket.
    #[cfg(test)]
    pub(crate) fn local_address(&self) -> Option<SocketAddr> {
        self.driver.local_address()
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
        let level = if self.client { Priority::L0 } else { priority };
        // Each level is full on its own, so a stream that waits on flow control never holds
        // back another.
        if self.outgoing[index(level)].frames.len() > 1 {
            return Ok(Offer::Full);
        }
        self.outgoing[index(level)].frames.push_back(frame.to_vec());
        Ok(Offer::Accepted)
    }

    fn flush(&mut self) -> Result<Flush, CarrierError> {
        self.rebind_if_needed();
        // Frames first, so acknowledgements of what `receive` took ride on them.
        self.write_streams()?;
        if self.closing || !self.client {
            // Acknowledgements and credit arrive with datagrams, and a server that stopped
            // receiving still needs them to finish writing. A server's bell is its own, so it
            // rings again for the frames `receive` has not read yet; a client takes datagrams
            // here only once nothing receives.
            if self.driver.ingest(Instant::now())
                && let Side::Server { route, .. } = &self.driver.side
            {
                ring(&route.ring);
            }
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
            let _ = self.driver.ingest(Instant::now());
            if let Err(error) = self.read_streams() {
                self.violation = Some(error);
            }
        }
        if let Some(frame) = self.inbound.pop_front() {
            self.inbound_bytes -= limits::HEADER_BYTES + frame.body.len();
            return Ok(Some(frame));
        }
        // Once the connection is gone nothing more can arrive before the goodbye.
        if self.goodbye.is_some() && (self.levels_ended() || self.driver.state.lost.is_some()) {
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
            write: self.driver.stashed_descriptor(),
            // Acknowledgements arrive on the same descriptor and free QUIC's buffers.
            progress: Some(descriptor),
            deadline: [
                self.driver.connection.poll_timeout(),
                self.path.as_ref().map(PathWatch::deadline),
            ]
            .into_iter()
            .flatten()
            .min(),
            signal: self.path.as_ref().and_then(PathWatch::descriptor),
        }
    }

    fn close(&mut self) {
        self.closing = true;
    }

    fn path_suspect(&mut self) {
        if self.client {
            let _ = self.driver.rebind(Instant::now());
        }
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
        let certificate = rcgen::CertificateParams::new(vec![SERVER_NAME.into()])
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
            ring(&listener.stop);
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
    fn routed_ipv4() -> Option<std::net::IpAddr> {
        let socket = UdpSocket::bind((std::net::Ipv4Addr::UNSPECIFIED, 0)).ok()?;
        socket
            .connect((std::net::Ipv4Addr::new(192, 0, 2, 1), 9))
            .ok()?;
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
        if let Some(routed) = routed_ipv4() {
            cases.push((loopback, routed));
            cases.push((routed, loopback));
        }
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

    /// A listener rebinds from one address to every interface at the same port. A port it can't
    /// take leaves it at the address it had.
    #[test]
    fn rebinding_to_every_interface_keeps_the_old_address_when_the_port_is_taken() {
        let config = server_config_for_tests();
        // Another test can hold the port at another address, so a taken port tries another.
        let (listener, wildcard) = (0..8)
            .find_map(|_| {
                let listener =
                    QuicListener::bind("127.0.0.1:0".parse().unwrap(), Arc::clone(&config))
                        .unwrap();
                let wildcard = SocketAddr::new(
                    std::net::Ipv4Addr::UNSPECIFIED.into(),
                    listener.address().port(),
                );
                listener
                    .rebind(wildcard)
                    .ok()
                    .map(|()| (listener, wildcard))
            })
            .expect("a port free on every interface");
        assert_eq!(listener.address(), wildcard);
        assert_eq!(listener.demux.shared.sockets().udp.len(), 2);
        // A taken port leaves the listener where it was.
        let taken = UdpSocket::bind("127.0.0.1:0").unwrap();
        let elsewhere = SocketAddr::new(
            std::net::Ipv4Addr::UNSPECIFIED.into(),
            taken.local_addr().unwrap().port(),
        );
        assert_eq!(listener.rebind(elsewhere), Err(QuicError::AddressInUse));
        assert_eq!(listener.address(), wildcard);
        assert_eq!(listener.demux.shared.sockets().udp.len(), 2);
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
}
