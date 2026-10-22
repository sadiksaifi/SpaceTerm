//! One bounded socket owner adapts WSS to the existing poll-driven byte transport.
use super::*;
use rustls::{
    ClientConfig, ClientConnection, ServerConfig, ServerConnection, StreamOwned,
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime},
};
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    os::{
        fd::{AsRawFd, RawFd},
        unix::net::UnixStream,
    },
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tungstenite::{Message, WebSocket, protocol::WebSocketConfig};

const CHUNK: usize = 64 * 1024;
const HANDSHAKE_LIMIT: Duration = Duration::from_secs(5);
const STALL_LIMIT: Duration = Duration::from_secs(10);

pub(super) enum Socket {
    Client(StreamOwned<ClientConnection, TcpStream>),
    Server(StreamOwned<ServerConnection, TcpStream>),
}
impl Socket {
    fn tcp(&self) -> &TcpStream {
        match self {
            Self::Client(s) => &s.sock,
            Self::Server(s) => &s.sock,
        }
    }
}
impl Read for Socket {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Client(s) => s.read(bytes),
            Self::Server(s) => s.read(bytes),
        }
    }
}
impl Write for Socket {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::Client(s) => s.write(bytes),
            Self::Server(s) => s.write(bytes),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::Client(s) => s.flush(),
            Self::Server(s) => s.flush(),
        }
    }
}
pub(super) type Ws = WebSocket<Socket>;

fn websocket_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .read_buffer_size(16 * 1024)
        .write_buffer_size(0)
        .max_write_buffer_size(2 * CHUNK)
        .max_message_size(Some(CHUNK))
        .max_frame_size(Some(CHUNK))
}

#[derive(Debug)]
struct Pinned {
    certificate: [u8; 32],
    mismatch: Arc<AtomicBool>,
}
impl rustls::client::danger::ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        cert: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        if !intermediates.is_empty() || crypto::hash(cert.as_ref()) != self.certificate {
            self.mismatch.store(true, Ordering::Release);
            return Err(rustls::Error::General("Pinned certificate mismatch".into()));
        }
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

pub(super) fn validate_endpoint(endpoint: &str) -> Result<SocketAddr, DirectError> {
    if endpoint.len() > 1024 {
        return Err(DirectError::InvalidAddress);
    }
    let uri: tungstenite::http::Uri = endpoint.parse().map_err(|_| DirectError::InvalidAddress)?;
    if uri.scheme_str() != Some("wss")
        || uri.authority().is_none_or(|a| a.as_str().contains('@'))
        || uri.path() != "/spaceterm"
        || uri.query().is_some()
    {
        return Err(DirectError::InvalidAddress);
    }
    let host = uri
        .host()
        .ok_or(DirectError::InvalidAddress)?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse()
        .map_err(|_| DirectError::InvalidAddress)?;
    Ok(SocketAddr::new(host, uri.port_u16().unwrap_or(443)))
}
pub(super) fn connect(endpoint: &str, pin: &ServerPin) -> Result<Ws, DirectError> {
    // Invitations advertise the same concrete IP policy the owner enabled. A synchronous
    // resolver would introduce an unbounded native wait outside the connection deadline.
    let address = validate_endpoint(endpoint)?;
    let deadline = Instant::now() + HANDSHAKE_LIMIT;
    let tcp = TcpStream::connect_timeout(&address, HANDSHAKE_LIMIT)
        .map_err(|_| DirectError::Transport)?;
    tcp.set_nonblocking(true)
        .map_err(|_| DirectError::Transport)?;
    let mismatch = Arc::new(AtomicBool::new(false));
    let config =
        ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|_| DirectError::Transport)?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(Pinned {
                certificate: pin.certificate,
                mismatch: Arc::clone(&mismatch),
            }))
            .with_no_client_auth();
    let name =
        ServerName::try_from("spaceterm.invalid").map_err(|_| DirectError::InvalidAddress)?;
    let mut connection =
        ClientConnection::new(Arc::new(config), name).map_err(|_| DirectError::Transport)?;
    connection.set_buffer_limit(Some(2 * CHUNK));
    let mut tcp = tcp;
    while connection.is_handshaking() {
        if Instant::now() >= deadline {
            return Err(DirectError::Transport);
        }
        match connection.complete_io(&mut tcp) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(2))
            }
            Err(_) if mismatch.load(Ordering::Acquire) => return Err(DirectError::PinMismatch),
            Err(_) => return Err(DirectError::AuthenticationRefused),
        }
    }
    let stream = Socket::Client(StreamOwned::new(connection, tcp));
    let mut handshake =
        tungstenite::client::client_with_config(endpoint, stream, Some(websocket_config()));
    loop {
        match handshake {
            Ok((ws, _)) => return Ok(ws),
            Err(tungstenite::HandshakeError::Interrupted(pending)) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(2));
                handshake = pending.handshake();
            }
            Err(_) => return Err(DirectError::Transport),
        }
    }
}
pub(super) fn accept(tcp: TcpStream, certificate: &[u8], key: &[u8]) -> Result<Ws, DirectError> {
    tcp.set_nonblocking(true)
        .map_err(|_| DirectError::Transport)?;
    let deadline = Instant::now() + HANDSHAKE_LIMIT;
    let config =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|_| DirectError::Transport)?
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(certificate.to_vec())],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.to_vec())),
            )
            .map_err(|_| DirectError::StorageUnavailable)?;
    let mut connection =
        ServerConnection::new(Arc::new(config)).map_err(|_| DirectError::Transport)?;
    connection.set_buffer_limit(Some(2 * CHUNK));
    let stream = Socket::Server(StreamOwned::new(connection, tcp));
    let mut handshake = tungstenite::accept_with_config(stream, Some(websocket_config()));
    loop {
        match handshake {
            Ok(ws) => return Ok(ws),
            Err(tungstenite::HandshakeError::Interrupted(pending)) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(2));
                handshake = pending.handshake();
            }
            Err(_) => return Err(DirectError::Transport),
        }
    }
}
pub(super) fn send(ws: &mut Ws, value: &impl Serialize) -> Result<(), DirectError> {
    let bytes = zeroize::Zeroizing::new(
        serde_json::to_vec(value).map_err(|_| DirectError::InvalidRequest)?,
    );
    if bytes.len() > wire::MAX_AUTH_FRAME {
        return Err(DirectError::Capacity);
    }
    match ws.write(Message::Binary(bytes.to_vec().into())) {
        Ok(()) => {}
        Err(error) if retryable(&error) => {}
        Err(_) => return Err(DirectError::Transport),
    }
    let deadline = Instant::now() + HANDSHAKE_LIMIT;
    loop {
        match ws.flush() {
            Ok(()) => return Ok(()),
            Err(error) if retryable(&error) && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(2))
            }
            Err(_) => return Err(DirectError::Transport),
        }
    }
}
pub(super) fn receive<T: serde::de::DeserializeOwned>(ws: &mut Ws) -> Result<T, DirectError> {
    let deadline = Instant::now() + HANDSHAKE_LIMIT;
    loop {
        if Instant::now() >= deadline {
            return Err(DirectError::Transport);
        }
        match ws.read() {
            Ok(Message::Binary(bytes)) if bytes.len() <= wire::MAX_AUTH_FRAME => {
                return serde_json::from_slice(&bytes).map_err(|_| DirectError::InvalidRequest);
            }
            Ok(Message::Ping(_) | Message::Pong(_)) => match ws.flush() {
                Ok(()) => {}
                Err(error) if retryable(&error) => {}
                Err(_) => return Err(DirectError::Transport),
            },
            Err(error) if retryable(&error) => std::thread::sleep(Duration::from_millis(2)),
            Err(_) => return Err(DirectError::Transport),
            _ => return Err(DirectError::InvalidRequest),
        }
    }
}

pub(super) struct Bridge {
    stream: UnixStream,
    socket: TcpStream,
    stop: Arc<AtomicBool>,
    draining: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<Result<(), DirectError>>>,
}
impl Bridge {
    pub(super) fn start(mut ws: Ws) -> Result<Self, DirectError> {
        let (stream, mut bridge) = UnixStream::pair().map_err(|_| DirectError::Transport)?;
        bridge
            .set_nonblocking(true)
            .map_err(|_| DirectError::Transport)?;
        ws.get_ref()
            .tcp()
            .set_nonblocking(true)
            .map_err(|_| DirectError::Transport)?;
        let socket = ws
            .get_ref()
            .tcp()
            .try_clone()
            .map_err(|_| DirectError::Transport)?;
        let stop = Arc::new(AtomicBool::new(false));
        let cancelled = Arc::clone(&stop);
        let draining = Arc::new(AtomicBool::new(false));
        let finish = Arc::clone(&draining);
        let worker = std::thread::spawn(move || {
            let result = pump(&mut ws, &mut bridge, &cancelled, &finish);
            let _ = bridge.shutdown(std::net::Shutdown::Both);
            let _ = ws.get_ref().tcp().shutdown(std::net::Shutdown::Both);
            result
        });
        Ok(Self {
            stream,
            socket,
            stop,
            draining,
            worker: Some(worker),
        })
    }
    pub(crate) fn stream(&self) -> &UnixStream {
        &self.stream
    }
    /// Retires an exchange after its final reply. EOF reaches the pump only after
    /// all queued reply bytes, and the pump flushes each WSS message before EOF.
    pub(super) fn finish_reply(&mut self) -> Result<(), DirectError> {
        self.draining.store(true, Ordering::Release);
        self.stream
            .shutdown(std::net::Shutdown::Write)
            .map_err(|_| DirectError::Transport)?;
        self.worker
            .take()
            .ok_or(DirectError::Transport)?
            .join()
            .map_err(|_| DirectError::Transport)?
    }
}
impl Drop for Bridge {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
        let _ = self.socket.shutdown(std::net::Shutdown::Both);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
impl Read for Bridge {
    fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
        self.stream.read(b)
    }
}
impl Write for Bridge {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.stream.write(b)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.stream.flush()
    }
}
impl crate::transport::Transport for Bridge {
    fn read_fd(&self) -> RawFd {
        self.stream.as_raw_fd()
    }
    fn write_fd(&self) -> RawFd {
        self.stream.as_raw_fd()
    }
}
fn retryable(error: &tungstenite::Error) -> bool {
    matches!(error, tungstenite::Error::Io(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted))
}
fn pump(
    ws: &mut Ws,
    bridge: &mut UnixStream,
    stop: &AtomicBool,
    draining: &AtomicBool,
) -> Result<(), DirectError> {
    let mut incoming = Vec::new();
    let mut offset = 0;
    let mut outgoing = false;
    let mut send_stalled = None;
    let mut receive_stalled = None;
    let mut buffer = vec![0; CHUNK];
    let mut drain_started = None;
    while !stop.load(Ordering::Acquire) {
        let finishing = draining.load(Ordering::Acquire);
        if finishing && drain_started.get_or_insert_with(Instant::now).elapsed() > STALL_LIMIT {
            return Err(DirectError::Transport);
        }
        let mut progress = false;
        if outgoing {
            match ws.flush() {
                Ok(()) => {
                    outgoing = false;
                    send_stalled = None;
                    progress = true;
                }
                Err(e) if retryable(&e) => {}
                Err(_) => return Err(DirectError::Transport),
            }
        } else {
            match bridge.read(&mut buffer) {
                Ok(0) => return Ok(()),
                Ok(count) => {
                    match ws.write(Message::Binary(buffer[..count].to_vec().into())) {
                        Ok(()) => {}
                        Err(e) if retryable(&e) => {} // tungstenite retained this message.
                        Err(_) => return Err(DirectError::Transport),
                    }
                    outgoing = true;
                    send_stalled = Some(Instant::now());
                    progress = true;
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(_) => return Err(DirectError::Transport),
            }
        }
        if finishing {
            // Only the final queued reply remains eligible. New input, including
            // pings, cannot keep the retired exchange alive or starve its EOF.
        } else if offset < incoming.len() {
            match bridge.write(&incoming[offset..]) {
                Ok(0) => return Err(DirectError::Transport),
                Ok(count) => {
                    offset += count;
                    progress = true;
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(_) => return Err(DirectError::Transport),
            }
        } else {
            incoming.clear();
            offset = 0;
            receive_stalled = None;
            match ws.read() {
                Ok(Message::Binary(bytes)) => {
                    incoming = bytes.to_vec();
                    receive_stalled = Some(Instant::now());
                    progress = true;
                }
                Ok(Message::Ping(_) | Message::Pong(_)) => {
                    outgoing = true;
                    send_stalled.get_or_insert_with(Instant::now);
                    progress = true;
                }
                Ok(Message::Close(_)) => return Ok(()),
                Ok(_) => return Err(DirectError::InvalidRequest),
                Err(e) if retryable(&e) => {}
                Err(_) => return Err(DirectError::Transport),
            }
        }
        if send_stalled.is_some_and(|at: Instant| at.elapsed() > STALL_LIMIT)
            || (!finishing && receive_stalled.is_some_and(|at: Instant| at.elapsed() > STALL_LIMIT))
        {
            return Err(DirectError::Transport);
        }
        if !progress {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    Ok(())
}
