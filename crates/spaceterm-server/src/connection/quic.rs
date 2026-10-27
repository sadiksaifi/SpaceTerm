//! The QUIC carrier prototype of #671: iroh on direct addresses only.
//!
//! - **No n0 services.** Endpoints use iroh's minimal preset: no relay, no address lookup and no
//!   port mapping, so nothing contacts n0's servers. A client dials the server's IP addresses.
//! - **Pinned keys.** An endpoint's iroh key is its pinned Ed25519 key: the server's pin public
//!   key, or the device credential. iroh's TLS proves both, so the client reaches only the pinned
//!   server and the server learns the device key before admission. Pairing proofs and admission
//!   still run on top, unchanged.
//! - **Streams.** L0 is the bidirectional stream the server opens for pairing and admission. Each
//!   side opens one unidirectional stream for L1 and one for L2 when it first sends at that level;
//!   such a stream starts with its level byte. Frames follow back to back with their length
//!   prefix, as on the Unix socket. A lost packet stalls only its own level.
//! - **Readiness.** iroh is asynchronous, so a tokio runtime with one worker lives inside this
//!   module. The connection's I/O thread never waits on it: tasks move frames through shared
//!   queues and ring a wake descriptor, which [`Carrier::readiness`] reports as readable.
use super::Wake;
use super::carrier::{Carrier, CarrierError, Flush, Offer, Priority, Readiness};
use crate::protocol::frame::FrameReader;
use crate::protocol::{RawFrame, limits};
use iroh::endpoint::{
    AckFrequencyConfig, Connection, Controller, ControllerFactory, QuicTransportConfig, RecvStream,
    RttEstimator, SendStream, VarInt, presets,
};
use iroh::{Endpoint, EndpointAddr, PublicKey, SecretKey};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::{Notify, mpsc};

/// The protocol's own ALPN, so no other iroh protocol is accepted on this endpoint.
pub const ALPN: &[u8] = b"spaceterm/2";
/// Received frame bytes that `receive` has not taken yet. Readers pause above it, so the peer's
/// sending waits on QUIC flow control.
const INBOUND_BYTES: usize = 2 * limits::MAX_FRAME_BYTES;
const READ_CHUNK: usize = 64 * 1024;
/// Handshakes the server runs at once, and connections that wait for the host to take them.
const MAX_PENDING: usize = 32;
const HANDSHAKE_LIMIT: Duration = Duration::from_secs(5);

/// Failures before the carrier exists. Classifications only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum QuicError {
    #[error("the QUIC runtime is unavailable")]
    Runtime,
    #[error("the key is not an Ed25519 key")]
    Key,
    #[error("the QUIC connection failed")]
    Transport,
    #[error("the QUIC connection timed out")]
    TimedOut,
}

fn runtime() -> Result<&'static tokio::runtime::Runtime, QuicError> {
    static RUNTIME: OnceLock<Option<tokio::runtime::Runtime>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .thread_name("spaceterm-quic")
                .enable_all()
                .build()
                .ok()
        })
        .as_ref()
        .ok_or(QuicError::Runtime)
}

/// Runs `future` on the runtime from a thread outside it, until `deadline`.
fn block_on<T>(
    deadline: Instant,
    future: impl Future<Output = Result<T, QuicError>>,
) -> Result<T, QuicError> {
    let runtime = runtime()?;
    runtime.block_on(async {
        tokio::time::timeout_at(deadline.into(), future)
            .await
            .unwrap_or(Err(QuicError::TimedOut))
    })
}

/// The iroh secret key of an Ed25519 PKCS#8 document of version 1 or 2, as ring writes it. It
/// must match `public`, the key the pin or pairing already holds.
pub fn secret_key(pkcs8: &[u8], public: &[u8; 32]) -> Result<SecretKey, QuicError> {
    // SEQUENCE { INTEGER version, AlgorithmIdentifier { id-Ed25519 }, OCTET STRING { OCTET
    // STRING seed } ... }: the seed's position is fixed for both versions.
    const ALGORITHM: [u8; 11] = [
        0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20,
    ];
    let header = pkcs8.get(..16).ok_or(QuicError::Key)?;
    if header[0] != 0x30
        || usize::from(header[1]) + 2 != pkcs8.len()
        || header[2..4] != [0x02, 0x01]
        || header[4] > 1
        || header[5..] != ALGORITHM
    {
        return Err(QuicError::Key);
    }
    let seed: [u8; 32] = pkcs8
        .get(16..48)
        .and_then(|seed| seed.try_into().ok())
        .ok_or(QuicError::Key)?;
    let secret = SecretKey::from_bytes(&seed);
    if secret.public().as_bytes() != public {
        return Err(QuicError::Key);
    }
    Ok(secret)
}

fn transport() -> QuicTransportConfig {
    // Terminal traffic is a few packets at a time, so most losses are tail losses that only a
    // probe timeout recovers. Asking the peer to acknowledge every packet at once keeps its
    // acknowledgement delay out of that timeout.
    let mut acknowledgements = AckFrequencyConfig::default();
    acknowledgements
        .ack_eliciting_threshold(VarInt::from_u32(0))
        .max_ack_delay(Some(Duration::from_millis(1)));
    QuicTransportConfig::builder()
        // The server opens the one L0 stream; each side opens one stream per other level.
        .max_concurrent_bidi_streams(VarInt::from_u32(1))
        .max_concurrent_uni_streams(VarInt::from_u32(2))
        .ack_frequency_config(Some(acknowledgements))
        .congestion_controller_factory(Arc::new(Floored(Arc::default())))
        .build()
}

/// Packets the congestion window always allows.
const FLOOR_PACKETS: u64 = 10;

/// Builds Cubic with a window of at least [`FLOOR_PACKETS`]. Cubic grows only while the window
/// limits sending, and terminal traffic rarely fills it, so random loss alone ratchets it down to
/// two packets and a screen of output then waits a round trip. The floor equals the usual initial
/// window.
#[derive(Debug)]
struct Floored(Arc<noq_proto::congestion::CubicConfig>);

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

async fn bind(
    secret: SecretKey,
    address: Option<SocketAddr>,
    alpns: Vec<Vec<u8>>,
) -> Result<Endpoint, QuicError> {
    let mut builder = Endpoint::builder(presets::Minimal)
        .secret_key(secret)
        .alpns(alpns)
        .transport_config(transport());
    if let Some(address) = address {
        builder = builder
            .clear_ip_transports()
            .bind_addr(address)
            .map_err(|_| QuicError::Transport)?;
    }
    builder.bind().await.map_err(|_| QuicError::Transport)
}

/// The L0 stream of a QUIC connection before admission: the pairing exchange and the admission
/// preamble. [`QuicStream::into_carrier`] turns it into the connection's carrier.
pub struct QuicStream {
    connection: Connection,
    send: SendStream,
    recv: RecvStream,
    /// A client's own endpoint, which lives as long as its connection.
    endpoint: Option<Endpoint>,
}

impl QuicStream {
    /// Dials the server whose iroh key is `server` at `addresses` and waits for its L0 stream.
    pub fn connect(
        secret: SecretKey,
        server: &[u8; 32],
        addresses: &[SocketAddr],
        deadline: Instant,
    ) -> Result<Self, QuicError> {
        let server = PublicKey::from_bytes(server).map_err(|_| QuicError::Key)?;
        let address = EndpointAddr::new(server).with_addrs(
            addresses
                .iter()
                .map(|address| iroh::TransportAddr::Ip(*address)),
        );
        block_on(deadline, async move {
            let endpoint = bind(secret, None, Vec::new()).await?;
            let connection = endpoint
                .connect(address, ALPN)
                .await
                .map_err(|_| QuicError::Transport)?;
            let (send, recv) = connection
                .accept_bi()
                .await
                .map_err(|_| QuicError::Transport)?;
            send.set_priority(level_priority(Priority::L0))
                .map_err(|_| QuicError::Transport)?;
            Ok(Self {
                connection,
                send,
                recv,
                endpoint: Some(endpoint),
            })
        })
    }

    /// The peer's iroh key, which its TLS handshake proved.
    pub fn remote_key(&self) -> [u8; 32] {
        *self.connection.remote_id().as_bytes()
    }

    /// Writes `bytes` before `deadline`.
    pub fn send_bytes(&mut self, bytes: &[u8], deadline: Instant) -> Result<(), QuicError> {
        block_on(deadline, async {
            self.send
                .write_all(bytes)
                .await
                .map_err(|_| QuicError::Transport)
        })
    }

    /// Reads exactly `bytes.len()` bytes before `deadline`.
    pub fn receive_exact(&mut self, bytes: &mut [u8], deadline: Instant) -> Result<(), QuicError> {
        block_on(deadline, async {
            self.recv
                .read_exact(bytes)
                .await
                .map_err(|_| QuicError::Transport)
        })
    }

    /// One length-prefixed message of at most `max` bytes, without its prefix.
    pub fn receive_prefixed(
        &mut self,
        max: usize,
        deadline: Instant,
    ) -> Result<Vec<u8>, QuicError> {
        let mut size = [0; 4];
        self.receive_exact(&mut size, deadline)?;
        let size = u32::from_be_bytes(size) as usize;
        if size > max {
            return Err(QuicError::Transport);
        }
        let mut bytes = vec![0; size];
        self.receive_exact(&mut bytes, deadline)?;
        Ok(bytes)
    }

    /// Starts the carrier's tasks on this connection.
    pub fn into_carrier(self) -> Result<IrohCarrier, QuicError> {
        IrohCarrier::start(self)
    }
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

/// The server's QUIC endpoint. A task accepts connections, completes their handshakes and opens
/// their L0 streams; the host takes them with [`QuicListener::accept`] after a wake.
pub struct QuicListener {
    endpoint: Endpoint,
    shared: Arc<Accepted>,
    task: tokio::task::JoinHandle<()>,
}

struct Accepted {
    ready: Mutex<VecDeque<QuicStream>>,
    wake: Mutex<Option<Arc<dyn Wake>>>,
    handshakes: Arc<tokio::sync::Semaphore>,
}

impl Accepted {
    fn lock(&self) -> MutexGuard<'_, VecDeque<QuicStream>> {
        self.ready.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl QuicListener {
    /// Binds UDP at `address` under the server's pinned key.
    pub fn bind(secret: SecretKey, address: SocketAddr) -> Result<Self, QuicError> {
        let endpoint = block_on(Instant::now() + HANDSHAKE_LIMIT, async move {
            bind(secret, Some(address), vec![ALPN.to_vec()]).await
        })?;
        let shared = Arc::new(Accepted {
            ready: Mutex::default(),
            wake: Mutex::default(),
            handshakes: Arc::new(tokio::sync::Semaphore::new(MAX_PENDING)),
        });
        let task = runtime()?.spawn(accept_loop(endpoint.clone(), Arc::clone(&shared)));
        Ok(Self {
            endpoint,
            shared,
            task,
        })
    }

    /// Rings `wake` whenever a connection becomes ready to take.
    pub fn set_wake(&self, wake: Arc<dyn Wake>) {
        *self.shared.wake.lock().unwrap_or_else(|e| e.into_inner()) = Some(wake);
    }

    /// The next connection whose handshake completed.
    pub fn accept(&self) -> Option<QuicStream> {
        self.shared.lock().pop_front()
    }
}

impl Drop for QuicListener {
    fn drop(&mut self) {
        self.task.abort();
        let endpoint = self.endpoint.clone();
        if let Ok(runtime) = runtime() {
            runtime.spawn(async move { endpoint.close().await });
        }
    }
}

async fn accept_loop(endpoint: Endpoint, shared: Arc<Accepted>) {
    while let Some(incoming) = endpoint.accept().await {
        let Ok(permit) = Arc::clone(&shared.handshakes).try_acquire_owned() else {
            incoming.refuse();
            continue;
        };
        let shared = Arc::clone(&shared);
        tokio::spawn(async move {
            let _permit = permit;
            let handshake = async {
                let connection = incoming.accept().ok()?.await.ok()?;
                let (send, recv) = connection.open_bi().await.ok()?;
                send.set_priority(level_priority(Priority::L0)).ok()?;
                Some(QuicStream {
                    connection,
                    send,
                    recv,
                    endpoint: None,
                })
            };
            let Ok(Some(stream)) = tokio::time::timeout(HANDSHAKE_LIMIT, handshake).await else {
                return;
            };
            {
                let mut ready = shared.lock();
                if ready.len() >= MAX_PENDING {
                    stream.connection.close(VarInt::from_u32(1), b"");
                    return;
                }
                ready.push_back(stream);
            }
            if let Some(wake) = &*shared.wake.lock().unwrap_or_else(|e| e.into_inner()) {
                wake.wake();
            }
        });
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

const LEVELS: [Priority; 3] = [Priority::L0, Priority::L1, Priority::L2];

fn index(priority: Priority) -> usize {
    match priority {
        Priority::L0 => 0,
        Priority::L1 => 1,
        Priority::L2 => 2,
    }
}

/// What the runtime's tasks and the carrier share.
struct Shared {
    state: Mutex<State>,
    /// Signalled when `receive` takes a frame, so a paused reader checks for room.
    room: Notify,
    ring: UnixStream,
}

#[derive(Default)]
struct State {
    inbound: VecDeque<RawFrame>,
    inbound_bytes: usize,
    /// Accepted frames not yet handed to QUIC.
    unflushed: usize,
    /// Frames handed to QUIC, so `flush` reports progress.
    written: u64,
    /// Writers that finished their stream after `close`.
    finished: usize,
    ended: Option<CarrierError>,
    /// The registry severed the connection: `receive` reports it closed.
    severed: bool,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn ring(&self) {
        // A full socket already wakes the thread, so a refused byte is harmless.
        let _ = (&self.ring).write(&[1]);
    }

    fn end(&self, error: CarrierError) {
        self.lock().ended.get_or_insert(error);
        self.ring();
    }
}

/// Stops a QUIC carrier from receiving, for revocation. Writing continues, so a queued goodbye
/// still leaves.
pub struct QuicSever(Arc<Shared>);

impl super::registry::Sever for QuicSever {
    fn sever(&self) {
        self.0.lock().severed = true;
        self.0.ring();
    }
}

pub struct IrohCarrier {
    shared: Arc<Shared>,
    /// One queue per level, in [`LEVELS`] order. Dropped by `close`, which finishes the streams.
    senders: Option<[mpsc::UnboundedSender<Vec<u8>>; 3]>,
    bell: UnixStream,
    connection: Connection,
    endpoint: Option<Endpoint>,
    /// `State::written` at the last flush.
    seen: u64,
    closing: bool,
}

impl IrohCarrier {
    fn start(stream: QuicStream) -> Result<Self, QuicError> {
        let runtime = runtime()?;
        let (bell, ring) = UnixStream::pair().map_err(|_| QuicError::Runtime)?;
        bell.set_nonblocking(true)
            .and_then(|()| ring.set_nonblocking(true))
            .map_err(|_| QuicError::Runtime)?;
        let shared = Arc::new(Shared {
            state: Mutex::default(),
            room: Notify::new(),
            ring,
        });
        let QuicStream {
            connection,
            send,
            recv,
            endpoint,
        } = stream;
        let mut first = Some(send);
        let senders = LEVELS.map(|level| {
            let (sender, frames) = mpsc::unbounded_channel();
            runtime.spawn(write_level(
                Arc::clone(&shared),
                connection.clone(),
                level,
                first.take().filter(|_| level == Priority::L0),
                frames,
            ));
            sender
        });
        runtime.spawn(read_stream(Arc::clone(&shared), connection.clone(), recv));
        runtime.spawn(accept_levels(Arc::clone(&shared), connection.clone()));
        // A stream's end is not the connection's, so one task watches the connection itself.
        let watched = connection.clone();
        let watcher = Arc::clone(&shared);
        runtime.spawn(async move {
            watched.closed().await;
            watcher.end(ended(&watched));
        });
        Ok(Self {
            shared,
            senders: Some(senders),
            bell,
            connection,
            endpoint,
            seen: 0,
            closing: false,
        })
    }

    /// A handle that severs this connection's receiving.
    pub fn sever(&self) -> QuicSever {
        QuicSever(Arc::clone(&self.shared))
    }

    fn drain_bell(&self) {
        let mut bytes = [0; 64];
        while matches!((&self.bell).read(&mut bytes), Ok(count) if count > 0) {}
    }
}

impl Drop for IrohCarrier {
    /// Closes the connection. After a drain the peer already read every stream to its end.
    fn drop(&mut self) {
        self.connection.close(VarInt::from_u32(0), b"");
        if let (Some(endpoint), Ok(runtime)) = (self.endpoint.take(), runtime()) {
            runtime.spawn(async move { endpoint.close().await });
        }
    }
}

impl Carrier for IrohCarrier {
    fn offer(&mut self, frame: &[u8], priority: Priority) -> Result<Offer, CarrierError> {
        if self.closing {
            return Err(CarrierError::Closed);
        }
        let Some(senders) = &self.senders else {
            return Err(CarrierError::Closed);
        };
        {
            let mut state = self.shared.lock();
            if let Some(error) = state.ended {
                return Err(error);
            }
            if state.unflushed > 1 {
                return Ok(Offer::Full);
            }
            state.unflushed += 1;
        }
        senders[index(priority)]
            .send(frame.to_vec())
            .map_err(|_| CarrierError::Failed)?;
        Ok(Offer::Accepted)
    }

    fn flush(&mut self) -> Result<Flush, CarrierError> {
        self.drain_bell();
        let state = self.shared.lock();
        let progressed = state.written != self.seen;
        self.seen = state.written;
        let pending = if self.closing {
            state.finished < LEVELS.len()
        } else {
            state.unflushed > 0
        };
        // Nothing accepted can leave a connection that ended.
        if pending && let Some(error) = state.ended {
            return Err(error);
        }
        Ok(Flush {
            progressed,
            pending,
        })
    }

    fn receive(&mut self) -> Result<Option<RawFrame>, CarrierError> {
        self.drain_bell();
        if self.closing {
            return Err(CarrierError::Closed);
        }
        let mut state = self.shared.lock();
        if state.severed {
            return Err(CarrierError::Closed);
        }
        if let Some(frame) = state.inbound.pop_front() {
            state.inbound_bytes -= limits::HEADER_BYTES + frame.body.len();
            drop(state);
            self.shared.room.notify_waiters();
            return Ok(Some(frame));
        }
        match state.ended {
            Some(error) => Err(error),
            None => Ok(None),
        }
    }

    fn readiness(&self) -> Readiness {
        let bell = self.bell.as_raw_fd();
        Readiness {
            read: bell,
            write: None,
            progress: Some(bell),
        }
    }

    fn close(&mut self) {
        self.closing = true;
        // The writers drain their queues, then finish their streams and wait until the peer read
        // them to the end.
        self.senders = None;
    }
}

/// Writes one level's frames in order. L0 already has its stream; L1 and L2 open theirs with
/// the first frame.
async fn write_level(
    shared: Arc<Shared>,
    connection: Connection,
    level: Priority,
    mut stream: Option<SendStream>,
    mut frames: mpsc::UnboundedReceiver<Vec<u8>>,
) {
    while let Some(frame) = frames.recv().await {
        if stream.is_none() {
            let opened = async {
                let mut send = connection.open_uni().await.ok()?;
                send.set_priority(level_priority(level)).ok()?;
                send.write_all(&[index(level) as u8]).await.ok()?;
                Some(send)
            };
            stream = opened.await;
        }
        let written = match &mut stream {
            Some(send) => send.write_all(&frame).await.is_ok(),
            None => false,
        };
        if !written {
            shared.end(ended(&connection));
            return;
        }
        {
            let mut state = shared.lock();
            state.unflushed -= 1;
            state.written += 1;
        }
        shared.ring();
    }
    if let Some(send) = &mut stream
        && send.finish().is_ok()
    {
        let _ = tokio::time::timeout(limits::STALL_AFTER, send.stopped()).await;
    }
    shared.lock().finished += 1;
    shared.ring();
}

/// Why a stream failed: the peer closed the connection, or it was lost.
fn ended(connection: &Connection) -> CarrierError {
    match connection.close_reason() {
        Some(
            iroh::endpoint::ConnectionError::ApplicationClosed(_)
            | iroh::endpoint::ConnectionError::LocallyClosed,
        ) => CarrierError::Closed,
        _ => CarrierError::Failed,
    }
}

/// Accepts the peer's L1 and L2 streams. A stream with another level byte, or a second stream
/// of one level, violates the protocol.
async fn accept_levels(shared: Arc<Shared>, connection: Connection) {
    let mut seen = [false; 3];
    while let Ok(mut recv) = connection.accept_uni().await {
        let mut level = [0];
        if recv.read_exact(&mut level).await.is_err() {
            return;
        }
        let level = level[0] as usize;
        if !matches!(level, 1 | 2) || seen[level] {
            shared.end(CarrierError::Decode(
                crate::protocol::DecodeError::Malformed,
            ));
            return;
        }
        seen[level] = true;
        tokio::spawn(read_stream(Arc::clone(&shared), connection.clone(), recv));
    }
}

/// Reads one stream's frames into the shared queue, pausing while the queue is full. A stream's
/// end is not the connection's: the peer closes the connection once every stream was read.
async fn read_stream(shared: Arc<Shared>, connection: Connection, mut recv: RecvStream) {
    let mut reader = FrameReader::default();
    let mut buffer = vec![0; READ_CHUNK];
    loop {
        loop {
            let room = shared.room.notified();
            if shared.lock().inbound_bytes < INBOUND_BYTES {
                break;
            }
            room.await;
        }
        let count = match recv.read(&mut buffer).await {
            Ok(Some(count)) => count,
            Ok(None) if reader.partial() => {
                shared.end(CarrierError::Decode(
                    crate::protocol::DecodeError::Malformed,
                ));
                return;
            }
            Ok(None) => return,
            Err(_) => {
                shared.end(ended(&connection));
                return;
            }
        };
        reader.feed(&buffer[..count]);
        let mut arrived = false;
        loop {
            match reader.next_frame() {
                Ok(Some(frame)) => {
                    let mut state = shared.lock();
                    state.inbound_bytes += limits::HEADER_BYTES + frame.body.len();
                    state.inbound.push_back(frame);
                    arrived = true;
                }
                Ok(None) => break,
                Err(error) => {
                    shared.end(CarrierError::Decode(error));
                    return;
                }
            }
        }
        if arrived {
            shared.ring();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_device_credential_becomes_the_iroh_key_of_its_own_public_key_only() {
        let credential = crate::direct::DeviceCredential::generate().unwrap();
        let public = credential.public_key().unwrap();
        let secret = secret_key(credential.expose_bytes(), &public).unwrap();
        assert_eq!(secret.public().as_bytes(), &public);
        let other = crate::direct::DeviceCredential::generate()
            .unwrap()
            .public_key()
            .unwrap();
        assert_eq!(
            secret_key(credential.expose_bytes(), &other).err(),
            Some(QuicError::Key)
        );
        assert_eq!(
            secret_key(&credential.expose_bytes()[..40], &public).err(),
            Some(QuicError::Key)
        );
    }
}
