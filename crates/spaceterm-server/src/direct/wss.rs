//! The paired WSS socket: the pinned TLS and WebSocket handshakes, the authentication messages
//! of pairing, and admission. Every wait is on socket readiness under an absolute deadline. After
//! admission the socket becomes a [`crate::connection::wss::WssCarrier`].
use super::*;
use crate::connection::io::wait_fd;
pub(crate) use crate::connection::wss::Ws;
use crate::connection::wss::{TlsSocket, websocket_config, would_block};
use rustls::{
    ClientConfig, ClientConnection, ServerConfig, ServerConnection,
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime},
};
use std::{
    net::TcpStream,
    os::fd::AsRawFd,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tungstenite::Message;

const HANDSHAKE_LIMIT: Duration = Duration::from_secs(5);

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

pub(super) fn validate_endpoint(endpoint: &str) -> Result<Endpoint, DirectError> {
    Endpoint::from_url(endpoint)
}

/// Waits for the socket under `deadline`: writable while TLS holds unsent bytes, else readable.
fn ready(socket: &mut TlsSocket, deadline: Instant) -> Result<(), DirectError> {
    let write = match std::io::Write::flush(socket) {
        Ok(()) => false,
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => true,
        Err(_) => return Err(DirectError::Transport),
    };
    if wait_fd(socket.tcp().as_raw_fd(), write, deadline) {
        Ok(())
    } else {
        Err(DirectError::Transport)
    }
}

fn prepare(tcp: &TcpStream) -> Result<(), DirectError> {
    tcp.set_nonblocking(true)
        .map_err(|_| DirectError::Transport)?;
    crate::connection::wss::tune(tcp).map_err(|_| DirectError::Transport)
}

pub(super) fn connect(endpoint: &str, pin: &ServerPin) -> Result<Ws, DirectError> {
    let address = validate_endpoint(endpoint)?;
    let deadline = Instant::now() + HANDSHAKE_LIMIT;
    let addresses = address.resolve(deadline)?;
    let mut connected = None;
    for address in addresses {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or(DirectError::Transport)?;
        if let Ok(tcp) = TcpStream::connect_timeout(&address, remaining.min(Duration::from_secs(1)))
        {
            connected = Some(tcp);
            break;
        }
    }
    let tcp = connected.ok_or(DirectError::Transport)?;
    prepare(&tcp)?;
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
    let connection =
        ClientConnection::new(Arc::new(config), name).map_err(|_| DirectError::Transport)?;
    let mut socket = TlsSocket::new(rustls::Connection::Client(connection), tcp);
    match socket.handshake(deadline) {
        Ok(()) => {}
        Err(_) if mismatch.load(Ordering::Acquire) => return Err(DirectError::PinMismatch),
        // TLS validation failures arrive as InvalidData; socket failures remain retryable.
        Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
            return Err(DirectError::AuthenticationRefused);
        }
        Err(_) => return Err(DirectError::Transport),
    }
    let mut handshake =
        tungstenite::client::client_with_config(endpoint, socket, Some(websocket_config()));
    loop {
        match handshake {
            Ok((ws, _)) => return Ok(ws),
            Err(tungstenite::HandshakeError::Interrupted(mut pending)) => {
                ready(pending.get_mut().get_mut(), deadline)?;
                handshake = pending.handshake();
            }
            Err(_) => return Err(DirectError::Transport),
        }
    }
}
pub(super) fn accept(tcp: TcpStream, certificate: &[u8], key: &[u8]) -> Result<Ws, DirectError> {
    prepare(&tcp)?;
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
    let connection = ServerConnection::new(Arc::new(config)).map_err(|_| DirectError::Transport)?;
    let mut socket = TlsSocket::new(rustls::Connection::Server(connection), tcp);
    socket
        .handshake(deadline)
        .map_err(|_| DirectError::Transport)?;
    let mut handshake = tungstenite::accept_with_config(socket, Some(websocket_config()));
    loop {
        match handshake {
            Ok(ws) => return Ok(ws),
            Err(tungstenite::HandshakeError::Interrupted(mut pending)) => {
                ready(pending.get_mut().get_mut(), deadline)?;
                handshake = pending.handshake();
            }
            Err(_) => return Err(DirectError::Transport),
        }
    }
}

/// Writes one binary message before `deadline`.
fn send_bytes(ws: &mut Ws, bytes: Vec<u8>, deadline: Instant) -> Result<(), DirectError> {
    match ws.write(Message::Binary(bytes.into())) {
        Ok(()) => {}
        Err(error) if would_block(&error) => {}
        Err(_) => return Err(DirectError::Transport),
    }
    loop {
        match ws.flush() {
            Ok(()) => return Ok(()),
            Err(error) if would_block(&error) => {
                if !wait_fd(ws.get_ref().tcp().as_raw_fd(), true, deadline) {
                    return Err(DirectError::Transport);
                }
            }
            Err(_) => return Err(DirectError::Transport),
        }
    }
}

/// Reads the next binary message of at most `max` bytes before `deadline`.
fn receive_bytes(ws: &mut Ws, max: usize, deadline: Instant) -> Result<Vec<u8>, DirectError> {
    loop {
        match ws.read() {
            Ok(Message::Binary(bytes)) if bytes.len() <= max => return Ok(bytes.to_vec()),
            Ok(Message::Ping(_) | Message::Pong(_)) => {}
            Err(error) if would_block(&error) => {
                if !ready_to_read(ws, deadline) {
                    return Err(DirectError::Transport);
                }
            }
            Err(_) => return Err(DirectError::Transport),
            _ => return Err(DirectError::InvalidRequest),
        }
    }
}

/// Waits to read, first writing any pong or TLS bytes still queued.
fn ready_to_read(ws: &mut Ws, deadline: Instant) -> bool {
    let write = match ws.flush() {
        Ok(()) => false,
        Err(error) => would_block(&error),
    };
    wait_fd(ws.get_ref().tcp().as_raw_fd(), write, deadline)
}

pub(super) fn send(ws: &mut Ws, value: &impl Serialize) -> Result<(), DirectError> {
    let bytes = zeroize::Zeroizing::new(
        serde_json::to_vec(value).map_err(|_| DirectError::InvalidRequest)?,
    );
    if bytes.len() > wire::MAX_AUTH_FRAME {
        return Err(DirectError::Capacity);
    }
    send_bytes(ws, bytes.to_vec(), Instant::now() + HANDSHAKE_LIMIT)
}
pub(super) fn receive<T: serde::de::DeserializeOwned>(ws: &mut Ws) -> Result<T, DirectError> {
    let bytes = zeroize::Zeroizing::new(receive_bytes(
        ws,
        wire::MAX_AUTH_FRAME,
        Instant::now() + HANDSHAKE_LIMIT,
    )?);
    serde_json::from_slice(&bytes).map_err(|_| DirectError::InvalidRequest)
}

/// Admission over WSS: the same length-prefixed bytes as account IPC, one frame per message. A
/// frame split across messages, as a peer of an older major may send it, is joined.
impl crate::protocol::Preamble for Ws {
    fn send(&mut self, frame: &[u8], deadline: Instant) -> Result<(), ServerError> {
        send_bytes(self, crate::protocol::prefixed(frame), deadline)
            .map_err(|_| ServerError::Protocol)
    }

    fn receive(&mut self, max: usize, deadline: Instant) -> Result<Vec<u8>, ServerError> {
        let mut bytes = Vec::new();
        loop {
            if let Some(prefix) = bytes.get(..4) {
                let size =
                    u32::from_be_bytes([prefix[0], prefix[1], prefix[2], prefix[3]]) as usize;
                if size > max {
                    return Err(ServerError::Protocol);
                }
                match (bytes.len() - 4).cmp(&size) {
                    std::cmp::Ordering::Equal => return Ok(bytes.split_off(4)),
                    std::cmp::Ordering::Greater => return Err(ServerError::Protocol),
                    std::cmp::Ordering::Less => {}
                }
            }
            let message = receive_bytes(self, max + 4 - bytes.len(), deadline)
                .map_err(|_| ServerError::Protocol)?;
            bytes.extend_from_slice(&message);
        }
    }
}
