//! Explicitly enabled TLS WebSockets with enrolled-key verification and binary-only messages.

mod server_tls;
pub use server_tls::server_tls_config;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use futures_util::stream::{FuturesUnordered, SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{CertificateError, DigitallySignedStruct, SignatureScheme};
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, Semaphore};
use tokio::time::Instant;
use tokio_rustls::{TlsAcceptor, TlsConnector, TlsStream};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::{Error as WebSocketError, Message};
use tokio_tungstenite::{WebSocketStream, accept_async_with_config, client_async_with_config};
use x509_parser::prelude::FromDer;

use crate::transport::{
    AdmissionLimits, ConnectionState, EnrolledServerKey, MessageLimit, MessageTransport, PeerFacts,
    PendingConnection, SendGuard, TransportError, TransportFuture, TransportRoute,
};

type Socket = WebSocketStream<TlsStream<crate::transport::ProgressIo<TcpStream>>>;

/// Explicit remote-access enablement, reconstructed only from the owning Server's durable setting.
///
/// This transport adapter does not write settings or issue user authorization. Composition
/// must persist the explicit choice before constructing this value. Discovery, pairing,
/// reconnect, repair, and boot must never synthesize enablement when that setting is absent.
#[derive(Clone)]
pub struct RemoteAccessEnablement {
    server_identity: [u8; 16],
    addresses: Vec<SocketAddr>,
}

impl std::fmt::Debug for RemoteAccessEnablement {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RemoteAccessEnablement(..)")
    }
}

impl RemoteAccessEnablement {
    /// Names the Server and exact IPv4/IPv6 bind addresses authorized by its durable setting.
    ///
    /// At most eight unique addresses are admitted. IPv6 listeners are IPv6-only: an IPv6
    /// wildcard never silently grants IPv4 reachability. Port zero permits native port selection.
    pub fn from_durable_setting(
        server_identity: [u8; 16],
        addresses: Vec<SocketAddr>,
    ) -> Result<Self, TransportError> {
        if server_identity == [0; 16]
            || addresses.is_empty()
            || addresses.len() > 8
            || addresses
                .iter()
                .enumerate()
                .any(|(index, address)| addresses[..index].contains(address))
        {
            return Err(TransportError::InvalidConfiguration);
        }
        Ok(Self {
            server_identity,
            addresses,
        })
    }

    /// Identifies the owning Server, without supplying authentication authority.
    pub const fn server_identity(&self) -> [u8; 16] {
        self.server_identity
    }
}

/// An explicitly enabled listener. Disabling wakes accepts and closes all its listening sockets.
pub struct WssListener {
    listeners: Mutex<Option<Vec<TcpListener>>>,
    state: ConnectionState,
    tls: TlsAcceptor,
    limit: MessageLimit,
    admission: AdmissionLimits,
    slots: Arc<Semaphore>,
}

impl WssListener {
    /// Binds only the explicitly enabled addresses. Without enablement no socket is created.
    ///
    /// Server TLS credentials are injected by native composition. Application authentication
    /// remains mandatory; TLS does not identify a Paired Device.
    pub async fn bind(
        enablement: Option<&RemoteAccessEnablement>,
        tls: Arc<rustls::ServerConfig>,
        limit: MessageLimit,
        admission: AdmissionLimits,
    ) -> Result<Self, TransportError> {
        let enablement = enablement.ok_or(TransportError::RemoteAccessDisabled)?;
        let _guard = crate::process::descriptor_guard().map_err(|_| TransportError::Unavailable)?;
        let mut listeners = Vec::with_capacity(enablement.addresses.len());
        for address in &enablement.addresses {
            let domain = if address.is_ipv4() {
                socket2::Domain::IPV4
            } else {
                socket2::Domain::IPV6
            };
            let socket =
                socket2::Socket::new(domain, socket2::Type::STREAM, Some(socket2::Protocol::TCP))
                    .map_err(|_| TransportError::Unavailable)?;
            if address.is_ipv6() {
                socket
                    .set_only_v6(true)
                    .map_err(|_| TransportError::Unavailable)?;
            }
            socket
                .set_nonblocking(true)
                .map_err(|_| TransportError::Unavailable)?;
            socket
                .bind(&(*address).into())
                .map_err(|_| TransportError::Unavailable)?;
            socket
                .listen(admission.connections as i32)
                .map_err(|_| TransportError::Unavailable)?;
            listeners.push(
                TcpListener::from_std(socket.into()).map_err(|_| TransportError::Unavailable)?,
            );
        }
        Ok(Self {
            listeners: Mutex::new(Some(listeners)),
            state: ConnectionState::new(),
            tls: TlsAcceptor::from(tls),
            limit,
            admission,
            slots: Arc::new(Semaphore::new(admission.connections)),
        })
    }

    /// Returns actual bind addresses for local discovery. They must not enter error diagnostics.
    pub async fn local_addresses(&self) -> Result<Vec<SocketAddr>, TransportError> {
        let listeners = self.listeners.lock().await;
        listeners
            .as_ref()
            .ok_or(TransportError::RemoteAccessDisabled)?
            .iter()
            .map(|listener| {
                listener
                    .local_addr()
                    .map_err(|_| TransportError::Unavailable)
            })
            .collect()
    }

    /// Accepts TLS and WebSocket negotiation under the same bounded application-handshake lease.
    pub async fn accept(&self) -> Result<PendingConnection<WssTransport>, TransportError> {
        self.state
            .run(async {
                let permit = Arc::clone(&self.slots)
                    .acquire_owned()
                    .await
                    .map_err(|_| TransportError::RemoteAccessDisabled)?;
                let (tcp, deadline) = {
                    let listeners = self.listeners.lock().await;
                    let listeners = listeners
                        .as_ref()
                        .ok_or(TransportError::RemoteAccessDisabled)?;
                    let mut pending = listeners
                        .iter()
                        .map(|listener| crate::process::descriptor_io(listener.accept()))
                        .collect::<FuturesUnordered<_>>();
                    let (tcp, _) = pending
                        .next()
                        .await
                        .ok_or(TransportError::RemoteAccessDisabled)?
                        .map_err(|_| TransportError::Unavailable)?;
                    (tcp, Instant::now() + self.admission.deadline)
                };
                let websocket = tokio::time::timeout_at(deadline, async {
                    let tls = self
                        .tls
                        .accept(crate::transport::ProgressIo::new(tcp))
                        .await
                        .map_err(|_| TransportError::TlsIdentityRejected)?;
                    accept_async_with_config(
                        TlsStream::Server(tls),
                        Some(websocket_config(self.limit)),
                    )
                    .await
                    .map_err(classify_websocket_error)
                })
                .await
                .map_err(|_| TransportError::HandshakeExpired)??;
                Ok(PendingConnection::new(
                    WssTransport::new(websocket, self.limit, None)?,
                    permit,
                    deadline,
                ))
            })
            .await
    }

    /// Revokes listening immediately and waits for in-flight accepts to release their sockets.
    /// Existing authenticated connections are owned by composition, not this listener.
    pub async fn disable(&self) {
        self.state.close(TransportError::RemoteAccessDisabled);
        self.slots.close();
        let mut listeners = self.listeners.lock().await;
        let _guard = crate::process::descriptor_guard();
        listeners.take();
    }
}

impl Drop for WssListener {
    fn drop(&mut self) {
        let _guard = crate::process::descriptor_guard();
        self.listeners.get_mut().take();
    }
}

fn websocket_config(limit: MessageLimit) -> WebSocketConfig {
    // Tungstenite has no compression support in this pinned configuration. Both the
    // individual frame and fragmented-message aggregate are bounded before allocation.
    WebSocketConfig::default()
        .read_buffer_size(8 * 1024)
        .write_buffer_size(0)
        .max_write_buffer_size(limit.bytes() + 256)
        .max_frame_size(Some(limit.bytes()))
        .max_message_size(Some(limit.bytes()))
        .accept_unmasked_frames(false)
}

fn classify_websocket_error(error: WebSocketError) -> TransportError {
    match error {
        WebSocketError::Capacity(_) | WebSocketError::WriteBufferFull(_) => {
            TransportError::MessageTooLarge
        }
        WebSocketError::ConnectionClosed | WebSocketError::AlreadyClosed => TransportError::Closed,
        WebSocketError::Io(_) => TransportError::Unavailable,
        _ => TransportError::WebSocketRejected,
    }
}

/// Full-duplex binary WebSocket transport. Client TLS trusts only the enrolled Ed25519 key.
pub struct WssTransport {
    writer: Mutex<Option<SplitSink<Socket, Message>>>,
    reader: Mutex<Option<SplitStream<Socket>>>,
    state: ConnectionState,
    limit: MessageLimit,
    facts: PeerFacts,
    write_progress: tokio::sync::watch::Receiver<Instant>,
}

impl WssTransport {
    fn new(
        socket: Socket,
        limit: MessageLimit,
        pin: Option<EnrolledServerKey>,
    ) -> Result<Self, TransportError> {
        let native = socket2::SockRef::from(socket.get_ref().get_ref().0.get_ref())
            .try_clone()
            .map_err(|_| TransportError::Unavailable)?;
        let write_progress = socket.get_ref().get_ref().0.progress();
        let (writer, reader) = socket.split();
        Ok(Self {
            writer: Mutex::new(Some(writer)),
            reader: Mutex::new(Some(reader)),
            write_progress,
            state: ConnectionState::with_socket(native),
            limit,
            facts: PeerFacts {
                route: TransportRoute::SecureWebSocket,
                local_uid: None,
                local_pid: None,
                pinned_server_key: pin,
            },
        })
    }

    /// Connects a supplied network route under a deadline, with no web-PKI fallback.
    ///
    /// The server name supplies SNI, not authority. A valid certificate for another key
    /// is rejected, while the exact enrolled key authenticates routes independently of DNS.
    pub async fn connect(
        address: SocketAddr,
        server_name: &str,
        pin: EnrolledServerKey,
        limit: MessageLimit,
        deadline: Duration,
    ) -> Result<Self, TransportError> {
        if server_name.len() > 253 || deadline.is_zero() || deadline > Duration::from_secs(60) {
            return Err(TransportError::InvalidConfiguration);
        }
        let name = ServerName::try_from(server_name.to_owned())
            .map_err(|_| TransportError::InvalidConfiguration)?;
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let configuration = rustls::ClientConfig::builder_with_provider(Arc::clone(&provider))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|_| TransportError::InvalidConfiguration)?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(PinnedServerVerifier { pin, provider }))
            .with_no_client_auth();
        let connector = TlsConnector::from(Arc::new(configuration));
        tokio::time::timeout(deadline, async {
            let tcp = crate::process::descriptor_io(TcpStream::connect(address))
                .await
                .map_err(|_| TransportError::Unavailable)?;
            let tls = connector
                .connect(name, crate::transport::ProgressIo::new(tcp))
                .await
                .map_err(|_| TransportError::TlsIdentityRejected)?;
            let (socket, response) = client_async_with_config(
                format!("wss://{address}/spaceterm"),
                TlsStream::Client(tls),
                Some(websocket_config(limit)),
            )
            .await
            .map_err(classify_websocket_error)?;
            if response.headers().contains_key("sec-websocket-extensions") {
                return Err(TransportError::WebSocketRejected);
            }
            Self::new(socket, limit, Some(pin))
        })
        .await
        .map_err(|_| TransportError::HandshakeExpired)?
    }
}

impl MessageTransport for WssTransport {
    fn write_progress(&self) -> Option<tokio::sync::watch::Receiver<Instant>> {
        Some(self.write_progress.clone())
    }
    fn peer_facts(&self) -> PeerFacts {
        self.facts
    }

    fn send<'a>(&'a self, message: &'a [u8]) -> TransportFuture<'a, ()> {
        Box::pin(async move {
            self.limit.check(message.len())?;
            let mut writer = self
                .state
                .run(async { Ok(self.writer.lock().await) })
                .await?;
            let writer = writer.as_mut().ok_or(TransportError::Closed)?;
            let mut guard = SendGuard::new(&self.state);
            let result = self
                .state
                .run(async {
                    writer
                        .send(Message::Binary(message.to_vec().into()))
                        .await
                        .map_err(classify_websocket_error)
                })
                .await;
            guard.complete();
            if let Err(error) = result {
                self.state.abort(error);
            }
            result
        })
    }

    fn recv(&self) -> TransportFuture<'_, Option<Vec<u8>>> {
        Box::pin(async move {
            if let Some(error) = self.state.error() {
                return if error == TransportError::Closed {
                    Ok(None)
                } else {
                    Err(error)
                };
            }
            let result = self
                .state
                .run(async {
                    let mut reader = self.reader.lock().await;
                    let reader = reader.as_mut().ok_or(TransportError::Closed)?;
                    loop {
                        match reader
                            .next()
                            .await
                            .transpose()
                            .map_err(classify_websocket_error)?
                        {
                            Some(Message::Binary(bytes)) => {
                                self.limit.check(bytes.len())?;
                                return Ok(Some(bytes.to_vec()));
                            }
                            Some(Message::Text(_)) | Some(Message::Frame(_)) => {
                                return Err(TransportError::WebSocketRejected);
                            }
                            Some(Message::Close(_)) | None => return Ok(None),
                            Some(Message::Ping(_)) | Some(Message::Pong(_)) => {
                                // The pinned Tungstenite reader coalesces automatic pongs,
                                // attempts their bounded writes on the next poll, and keeps
                                // reading on WouldBlock. Awaiting the writer or its flush here
                                // would couple ingress to backpressured application egress.
                            }
                        }
                    }
                })
                .await;
            match &result {
                Ok(None) => {
                    let _ = self.close().await;
                }
                Err(TransportError::Closed)
                    if self.state.error() == Some(TransportError::Closed) => {}
                Err(error) => {
                    self.state.abort(*error);
                    let _ = self.close().await;
                }
                _ => {}
            }
            if result == Err(TransportError::Closed) {
                Ok(None)
            } else {
                result
            }
        })
    }

    fn close(&self) -> TransportFuture<'_, ()> {
        Box::pin(async move {
            self.state.close(TransportError::Closed);
            let writer = self.writer.lock().await.take();
            let reader = self.reader.lock().await.take();
            self.state.release_socket();
            if let (Some(writer), Some(reader)) = (writer, reader) {
                let mut socket = writer
                    .reunite(reader)
                    .map_err(|_| TransportError::Unavailable)?;
                // A peer that stops reading cannot retain a closing connection indefinitely.
                let _ = tokio::time::timeout(Duration::from_secs(1), socket.close(None)).await;
                let _ =
                    tokio::time::timeout(Duration::from_secs(1), socket.get_mut().shutdown()).await;
            }
            Ok(())
        })
    }
}

#[derive(Debug)]
struct PinnedServerVerifier {
    pin: EnrolledServerKey,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl ServerCertVerifier for PinnedServerVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let reject =
            || rustls::Error::InvalidCertificate(CertificateError::ApplicationVerificationFailure);
        if end_entity.len() > 16 * 1024 || !intermediates.is_empty() || !ocsp_response.is_empty() {
            return Err(reject());
        }
        let (remaining, certificate) =
            x509_parser::certificate::X509Certificate::from_der(end_entity.as_ref())
                .map_err(|_| reject())?;
        let key = certificate.public_key();
        let now = i64::try_from(now.as_secs()).map_err(|_| reject())?;
        if !remaining.is_empty()
            || key.algorithm.algorithm != x509_parser::oid_registry::OID_SIG_ED25519
            || key.algorithm.parameters.is_some()
            || key.subject_public_key.unused_bits != 0
            || key.subject_public_key.data.as_ref() != self.pin.0
            || now < certificate.validity().not_before.timestamp()
            || now > certificate.validity().not_after.timestamp()
        {
            return Err(reject());
        }
        // Trust comes from the enrolled key; rustls separately verifies its live TLS
        // CertificateVerify signature. Self-signed certificate renewal keeps the same pin.
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            signature,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            signature,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ED25519]
    }
}
