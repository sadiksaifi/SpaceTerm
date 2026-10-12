//! Bounded byte-message contracts. Peer facts never establish Paired Device authority.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use thiserror::Error;
use tokio::sync::{OwnedSemaphorePermit, watch};
use tokio::time::Instant;

/// Largest admitted serialized envelope, including its authentication overhead.
pub const MAX_ENVELOPE_BYTES: usize = 2 * 1024 * 1024;

/// Content-free transport failure classifications.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum TransportError {
    /// The supplied bound, path, argument, or native configuration is invalid.
    #[error("transport configuration is invalid")]
    InvalidConfiguration,
    /// A message exceeds the admitted byte bound.
    #[error("transport message exceeds its byte bound")]
    MessageTooLarge,
    /// Framing is invalid or a message ended before its declared length.
    #[error("transport message framing is invalid")]
    InvalidFrame,
    /// The connection has closed or an interrupted write made it unusable.
    #[error("transport connection is closed")]
    Closed,
    /// Native I/O could not complete. No native error is retained.
    #[error("transport I/O is unavailable")]
    Unavailable,
    /// A private directory, endpoint, or retained identity is unsafe or changed.
    #[error("private transport endpoint is unsafe or changed")]
    UnsafeEndpoint,
    /// Native peer credentials do not identify the current account.
    #[error("transport peer account is ineligible")]
    PeerAccountRejected,
    /// The unauthenticated connection deadline expired.
    #[error("transport authentication deadline expired")]
    HandshakeExpired,
    /// An explicit durable remote-access setting is absent or was disabled.
    #[error("remote access is disabled")]
    RemoteAccessDisabled,
    /// TLS did not prove possession of the enrolled Server public key.
    #[error("pinned transport identity could not be verified")]
    TlsIdentityRejected,
    /// WebSocket negotiation or message kind is not supported.
    #[error("WebSocket transport protocol is invalid")]
    WebSocketRejected,
    /// The owned SSH process ended before its forwarding endpoint became ready.
    #[error("SSH tunnel could not become ready")]
    TunnelExited,
    /// The supplied AskPass adapter could not issue a handoff.
    #[error("SSH authentication handoff is unavailable")]
    AskPassUnavailable,
}

/// A validated per-message limit. Zero-length messages are not protocol envelopes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MessageLimit(usize);

impl MessageLimit {
    /// Admits a caller's limit up to the envelope ceiling.
    pub fn new(bytes: usize) -> Result<Self, TransportError> {
        if bytes == 0 || bytes > MAX_ENVELOPE_BYTES {
            return Err(TransportError::InvalidConfiguration);
        }
        Ok(Self(bytes))
    }

    /// The maximum authenticated-envelope bound.
    pub const fn envelope() -> Self {
        Self(MAX_ENVELOPE_BYTES)
    }

    /// Returns the admitted byte count.
    pub const fn bytes(self) -> usize {
        self.0
    }

    pub(crate) fn check(self, bytes: usize) -> Result<(), TransportError> {
        if bytes == 0 {
            Err(TransportError::InvalidFrame)
        } else if bytes > self.0 {
            Err(TransportError::MessageTooLarge)
        } else {
            Ok(())
        }
    }
}

/// Listener admission bounds, including TLS and the application authentication exchange.
#[derive(Clone, Copy, Debug)]
pub struct AdmissionLimits {
    pub(crate) connections: usize,
    pub(crate) deadline: Duration,
}

impl AdmissionLimits {
    /// Admits at most 128 unauthenticated connections with deadlines of at most 60 seconds.
    pub fn new(connections: usize, deadline: Duration) -> Result<Self, TransportError> {
        if connections == 0
            || connections > 128
            || deadline.is_zero()
            || deadline > Duration::from_secs(60)
        {
            return Err(TransportError::InvalidConfiguration);
        }
        Ok(Self {
            connections,
            deadline,
        })
    }
}

/// Reachability mechanism, independent of protocol authentication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportRoute {
    /// Same-account private Unix-domain socket, including SSH stream-local forwarding.
    PrivateIpc,
    /// TLS WebSocket carrying binary protocol envelopes.
    SecureWebSocket,
}

/// An Ed25519 Server public key obtained from enrollment, not from the current connection.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct EnrolledServerKey(pub [u8; 32]);

impl std::fmt::Debug for EnrolledServerKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("EnrolledServerKey(..)")
    }
}

/// Facts observed by a transport. None supplies Paired Device authentication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PeerFacts {
    /// The route used for this connection.
    pub route: TransportRoute,
    /// Native account UID when a private local peer was verified.
    pub local_uid: Option<u32>,
    /// Native process ID where the host provides it.
    pub local_pid: Option<u32>,
    /// The enrolled key proven by a client-side TLS handshake, when applicable.
    pub pinned_server_key: Option<EnrolledServerKey>,
}

/// A Send future at the object-safe transport boundary.
pub type TransportFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, TransportError>> + Send + 'a>>;

/// Whole, nonempty byte messages with direct socket backpressure and no application queue.
///
/// Sending and receiving may run concurrently. Calls in each direction serialize.
/// A cancelled receive preserves partial framing. Cancelling an active write closes
/// the connection because a partial envelope cannot be retried on the same byte stream.
/// Cancelling while waiting for the write lock leaves that connection intact.
/// Composition owns bounded egress queues and protocol authentication above this layer.
pub trait MessageTransport: Send + Sync {
    /// Reports native eligibility and TLS pin facts, never authenticated application authority.
    fn peer_facts(&self) -> PeerFacts;
    /// Sends one whole message, awaiting socket backpressure.
    fn send<'a>(&'a self, message: &'a [u8]) -> TransportFuture<'a, ()>;
    /// Receives one whole message, or `None` after orderly shutdown.
    fn recv(&self) -> TransportFuture<'_, Option<Vec<u8>>>;
    /// Closes the connection and wakes blocked operations. Repeated closes are harmless.
    fn close(&self) -> TransportFuture<'_, ()>;
}

pub(crate) struct ConnectionState {
    closed: watch::Sender<Option<TransportError>>,
    socket: Mutex<Option<socket2::Socket>>,
}

impl ConnectionState {
    pub(crate) fn new() -> Self {
        Self {
            closed: watch::channel(None).0,
            socket: Mutex::new(None),
        }
    }

    pub(crate) fn with_socket(socket: socket2::Socket) -> Self {
        Self {
            closed: watch::channel(None).0,
            socket: Mutex::new(Some(socket)),
        }
    }

    pub(crate) fn abort(&self, error: TransportError) {
        self.close(error);
        if let Ok(mut socket) = self.socket.lock()
            && let Some(socket) = socket.take()
        {
            let _ = socket.shutdown(std::net::Shutdown::Both);
        }
    }

    pub(crate) fn release_socket(&self) {
        if let Ok(mut socket) = self.socket.lock() {
            socket.take();
        }
    }

    pub(crate) fn error(&self) -> Option<TransportError> {
        *self.closed.borrow()
    }

    pub(crate) fn close(&self, error: TransportError) {
        self.closed.send_if_modified(|value| {
            if value.is_some() {
                return false;
            }
            *value = Some(error);
            true
        });
    }

    pub(crate) async fn run<T>(
        &self,
        operation: impl Future<Output = Result<T, TransportError>>,
    ) -> Result<T, TransportError> {
        let mut closed = self.closed.subscribe();
        tokio::select! {
            biased;
            result = closed.wait_for(Option::is_some) => {
                Err(result.ok().and_then(|value| *value).unwrap_or(TransportError::Closed))
            }
            result = operation => result,
        }
    }
}

pub(crate) struct SendGuard<'a> {
    state: &'a ConnectionState,
    complete: bool,
}

impl<'a> SendGuard<'a> {
    pub(crate) fn new(state: &'a ConnectionState) -> Self {
        Self {
            state,
            complete: false,
        }
    }

    pub(crate) fn complete(&mut self) {
        self.complete = true;
    }
}

impl Drop for SendGuard<'_> {
    fn drop(&mut self) {
        if !self.complete {
            self.state.abort(TransportError::Closed);
        }
    }
}

struct AdmissionLease {
    status: AtomicU8,
    permit: Mutex<Option<OwnedSemaphorePermit>>,
}

impl AdmissionLease {
    fn release(&self) {
        if let Ok(mut permit) = self.permit.lock() {
            permit.take();
        }
    }
}

/// An eligible, not yet application-authenticated connection with a live admission deadline.
///
/// The timer closes idle connections and releases their admission slot even if no caller
/// polls them. Dropping a pending connection also releases its slot and cancels its timer.
pub struct PendingConnection<T: MessageTransport + 'static> {
    transport: Arc<T>,
    lease: Arc<AdmissionLease>,
    deadline: Instant,
    timer: tokio::task::JoinHandle<()>,
}

impl<T: MessageTransport + 'static> PendingConnection<T> {
    pub(crate) fn new(transport: T, permit: OwnedSemaphorePermit, deadline: Instant) -> Self {
        let transport = Arc::new(transport);
        let lease = Arc::new(AdmissionLease {
            status: AtomicU8::new(0),
            permit: Mutex::new(Some(permit)),
        });
        let timer_transport = Arc::clone(&transport);
        let timer_lease = Arc::clone(&lease);
        let timer = tokio::spawn(async move {
            tokio::time::sleep_until(deadline).await;
            if timer_lease
                .status
                .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                let _ = timer_transport.close().await;
                timer_lease.release();
            }
        });
        Self {
            transport,
            lease,
            deadline,
            timer,
        }
    }

    fn check_deadline(&self) -> Result<(), TransportError> {
        if Instant::now() >= self.deadline || self.lease.status.load(Ordering::Acquire) == 2 {
            Err(TransportError::HandshakeExpired)
        } else {
            Ok(())
        }
    }

    /// Releases the admission slot after composition verifies the pinned Server and Paired Device.
    ///
    /// This operation does not verify proofs. Calling it before successful protocol
    /// authentication is a composition error; local account eligibility alone is insufficient.
    pub fn finish_authentication(self) -> Result<Arc<T>, TransportError> {
        self.check_deadline()?;
        self.lease
            .status
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| TransportError::HandshakeExpired)?;
        self.lease.release();
        Ok(Arc::clone(&self.transport))
    }
}

impl<T: MessageTransport + 'static> Drop for PendingConnection<T> {
    fn drop(&mut self) {
        self.timer.abort();
        self.lease.release();
    }
}

impl<T: MessageTransport + 'static> MessageTransport for PendingConnection<T> {
    fn peer_facts(&self) -> PeerFacts {
        self.transport.peer_facts()
    }

    fn send<'a>(&'a self, message: &'a [u8]) -> TransportFuture<'a, ()> {
        Box::pin(async move {
            self.check_deadline()?;
            tokio::time::timeout_at(self.deadline, self.transport.send(message))
                .await
                .map_err(|_| TransportError::HandshakeExpired)?
        })
    }

    fn recv(&self) -> TransportFuture<'_, Option<Vec<u8>>> {
        Box::pin(async move {
            self.check_deadline()?;
            tokio::time::timeout_at(self.deadline, self.transport.recv())
                .await
                .map_err(|_| TransportError::HandshakeExpired)?
        })
    }

    fn close(&self) -> TransportFuture<'_, ()> {
        Box::pin(async move {
            self.lease.status.store(2, Ordering::Release);
            let result = self.transport.close().await;
            self.lease.release();
            result
        })
    }
}
