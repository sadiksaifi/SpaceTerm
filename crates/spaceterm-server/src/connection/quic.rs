//! The QUIC carrier: noq-proto, n0's fork of quinn's protocol state machine, driven on the
//! connection's own I/O thread.
//!
//! - **No runtime.** noq-proto does no I/O. Each connection's thread reads and writes UDP itself,
//!   so a frame crosses no other thread on the client. On the server the listener's sockets, one
//!   per address family on every interface, carry every connection: a thread of the listener
//!   reads them and hands each datagram to its connection's thread, which sends its own
//!   datagrams directly.
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
//! - **Draining.** A server connection whose thread ends drains on the listener's thread, which
//!   sends the close again when the peer's packets show it was lost, so a lost close never reads
//!   as a stateless reset.
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
use std::mem::ManuallyDrop;
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
/// Closed server connections the listener keeps draining at once. Past it a closed connection is
/// forgotten at once, and a peer that lost its close sees a stateless reset.
const MAX_DRAINING: usize = 64;
/// How long a closed server connection drains: at least QUIC's three probe timeouts, and at least
/// [`DRAIN_MIN`], so a peer whose probe timer backed off while the close was lost still reaches
/// it, but never past [`DRAIN_MAX`].
const DRAIN_MIN: Duration = Duration::from_secs(1);
const DRAIN_MAX: Duration = Duration::from_secs(3);
/// Datagrams a draining connection sends in answer to its peer: QUIC resends the close for every
/// packet that arrives, and this bounds what a peer can make the server send.
const DRAIN_SENDS: usize = 8;
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
    /// An old address a failed widening could neither replace nor bind again. The listener's
    /// thread binds it again at each turn until it can.
    lost: Option<SocketAddr>,
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
    fn widen(&mut self) -> Result<(), QuicError> {
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

    fn for_destination(&self, destination: SocketAddr) -> Option<&Udp> {
        self.udp
            .iter()
            .find(|udp| udp.ipv4 == destination.is_ipv4())
    }
}

/// How long a server connection waits before it sends a datagram again that its listener's full
/// socket refused.
const STASH_RETRY: Duration = Duration::from_millis(2);

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
    fn drain(&mut self, handle: ConnectionHandle, connection: Connection, now: Instant) {
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
    fn expire(&mut self, now: Instant) {
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

    fn forget(&mut self, handle: ConnectionHandle) {
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
    /// Wakes the listener's thread: to stop, to widen, or to drain a connection whose thread
    /// ended.
    ring: UnixStream,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Server> {
        self.server.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Hands a closed connection whose thread ends to the listener's thread.
    fn drain(&self, handle: ConnectionHandle, connection: Connection, now: Instant) {
        self.lock().drain(handle, connection, now);
        ring(&self.ring);
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

    fn widen(&self) -> Result<(), QuicError> {
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
    wake: Mutex<Option<Arc<dyn super::Wake>>>,
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
    pub fn set_wake(&self, wake: Arc<dyn super::Wake>) {
        *self.demux.wake.lock().unwrap_or_else(|e| e.into_inner()) = Some(wake);
    }

    /// The address and port the listener is bound to. Every interface reads as `0.0.0.0`.
    pub fn address(&self) -> SocketAddr {
        self.demux.shared.sockets().address
    }

    /// Widens the listener from one address to every interface at the same port and keeps its
    /// connections, as when exposure widens. When the wildcard can't be bound the listener keeps
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
            && server.draining.is_empty()
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
    /// When a server connection tries its stashed datagram again.
    retry: Option<Instant>,
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
            retry: None,
            transmit: Vec::new(),
        }
    }
}

/// One QUIC connection and its I/O, driven only when its thread calls it.
struct Driver {
    /// Taken only by `Drop`, which hands a server's closed connection to the listener.
    connection: ManuallyDrop<Connection>,
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

    /// The descriptor that turns writable once a client's stashed datagram can be sent.
    fn stashed_descriptor(&self) -> Option<RawFd> {
        self.state.stashed.as_ref()?;
        match &self.side {
            Side::Client { socket, .. } => Some(socket.fd()),
            // A poll on the listener's socket keeps it open on Linux, so a widening listener
            // couldn't bind its port while this connection waited. It tries again on a deadline.
            Side::Server { .. } => None,
        }
    }

    /// When a server connection tries its stashed datagram again.
    fn stash_deadline(&self) -> Option<Instant> {
        match &self.side {
            Side::Server { .. } if self.state.stashed.is_some() => self.state.retry,
            _ => None,
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
                    // later stateless reset replaces it. A server past its draining bound forgets
                    // a connection as soon as it closes, so its reset to this end's next packet
                    // can arrive in the same batch as its close, and the TLS alert of a refusal
                    // would be lost.
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
                    self.state.retry = Some(now + STASH_RETRY);
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
                    self.state.retry = Some(now + STASH_RETRY);
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
    /// A second handle on a server connection's bell. A client has none.
    fn ring(&self) -> Option<UnixStream> {
        match &self.side {
            Side::Server { route, .. } => route.ring.try_clone().ok(),
            Side::Client { .. } => None,
        }
    }

    fn wait(&self, deadline: Instant) -> Result<(), QuicError> {
        let now = Instant::now();
        if now >= deadline {
            return Err(QuicError::TimedOut);
        }
        let until = [self.connection.poll_timeout(), self.stash_deadline()]
            .into_iter()
            .flatten()
            .fold(deadline, Instant::min);
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
    /// Closes the connection and sends the close. A server's connection then drains on the
    /// listener's thread, which sends the close again whenever the peer's packets show it was
    /// lost.
    fn drop(&mut self) {
        let now = Instant::now();
        if !self.connection.is_closed() {
            self.connection
                .close(now, VarInt::from_u32(0), bytes::Bytes::new());
        }
        self.transmit(now);
        // A connection that already drained told its endpoint here, so it is told only once.
        self.endpoint_events();
        // SAFETY: `drop` runs once, and nothing reads the field after this.
        let connection = unsafe { ManuallyDrop::take(&mut self.connection) };
        if let Side::Server { shared, .. } = &self.side {
            shared.drain(self.handle, connection, now);
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
    /// Set by [`Self::sever`]'s handle. The carrier keeps it.
    severed: Arc<std::sync::atomic::AtomicBool>,
}

impl QuicStream {
    fn new(driver: Driver) -> Self {
        Self {
            driver,
            l0: None,
            read: Vec::new(),
            severed: Arc::default(),
        }
    }

    /// A handle that ends this connection's L0 reads and writes, and later its carrier's
    /// receiving. A server connection's bell wakes a read waiting on it.
    pub fn sever(&self) -> QuicSever {
        QuicSever {
            severed: Arc::clone(&self.severed),
            ring: self.driver.ring(),
        }
    }

    fn check_severed(&self) -> Result<(), QuicError> {
        if self.severed.load(std::sync::atomic::Ordering::Acquire) {
            Err(QuicError::Closed)
        } else {
            Ok(())
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
                connection: ManuallyDrop::new(connection),
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
            self.check_severed()?;
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
            self.check_severed()?;
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

    /// A service-lane frame is larger than the admission window. Reading a frame that exceeds
    /// the window would wait for bytes the peer cannot send until the window grows.
    fn admitted(&mut self) {
        self.driver
            .connection
            .set_receive_window(VarInt::from_u32(ADMITTED_WINDOW));
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
            severed,
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
            severed,
            sent: 0,
            closing: false,
            violation: None,
        }
    }

    /// A handle that severs this connection's receiving. A server connection's bell wakes its
    /// thread.
    pub fn sever(&self) -> QuicSever {
        QuicSever {
            severed: Arc::clone(&self.severed),
            ring: self.driver.ring(),
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
                self.driver.stash_deadline(),
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
                SERVER_NAME,
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
    fn a_server_connection_with_a_refused_datagram_lets_its_listener_widen() {
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
            .with_custom_certificate_verifier(Arc::new(AnyServer))
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
                .wait(Instant::now() + Duration::from_millis(500));
            carrier
        });
        assert_eq!(listener.widen(), Ok(()));
        assert_eq!(listener.demux.shared.sockets().udp.len(), 2);
        drop(waiting.join().unwrap());
        // The client waits for bytes on L0 that never come.
        let _ = client.join();
    }

    /// Accepts any server certificate, for tests of the transport alone.
    #[derive(Debug)]
    struct AnyServer;

    impl rustls::client::danger::ServerCertVerifier for AnyServer {
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
}
