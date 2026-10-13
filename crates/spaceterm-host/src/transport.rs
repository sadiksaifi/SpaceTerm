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
    /// Observes successful native byte writes, not buffered application/TLS acceptance.
    /// Adapters without a native feed retain a conservative send deadline.
    fn write_progress(&self) -> Option<watch::Receiver<Instant>> {
        None
    }
    /// Receives one whole message, or `None` after orderly shutdown.
    fn recv(&self) -> TransportFuture<'_, Option<Vec<u8>>>;
    /// Closes the connection and wakes blocked operations. Repeated closes are harmless.
    fn close(&self) -> TransportFuture<'_, ()>;
}

/// Observe the socket below framing and TLS so buffering cannot reset a deadline.
pub(crate) struct ProgressIo<T> {
    inner: T,
    progress: watch::Sender<Instant>,
}
impl<T> ProgressIo<T> {
    pub(crate) fn new(inner: T) -> Self {
        Self {
            inner,
            progress: watch::channel(Instant::now()).0,
        }
    }
    pub(crate) fn get_ref(&self) -> &T {
        &self.inner
    }
    pub(crate) fn progress(&self) -> watch::Receiver<Instant> {
        self.progress.subscribe()
    }
    fn observe(&self, result: &std::task::Poll<std::io::Result<usize>>) {
        if matches!(result, std::task::Poll::Ready(Ok(bytes)) if *bytes != 0) {
            self.progress.send_replace(Instant::now());
        }
    }
}
impl<T: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for ProgressIo<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buffer: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_read(cx, buffer)
    }
}
impl<T: tokio::io::AsyncWrite + Unpin> tokio::io::AsyncWrite for ProgressIo<T> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        bytes: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.inner).poll_write(cx, bytes);
        this.observe(&result);
        result
    }
    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        bytes: &[std::io::IoSlice<'_>],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.inner).poll_write_vectored(cx, bytes);
        this.observe(&result);
        result
    }
    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
    fn poll_flush(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }
    fn poll_shutdown(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
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

struct PendingConnectionState<T> {
    // Field order destroys an abandoned transport before returning its admission slot.
    transport: Arc<T>,
    lease: AdmissionLease,
}

/// An eligible, not yet application-authenticated connection with a live admission deadline.
///
/// The timer closes idle connections and releases their admission slot even if no caller
/// polls them. Drop cancels the timer and holds admission until the transport is destroyed.
pub struct PendingConnection<T: MessageTransport + 'static> {
    state: Arc<PendingConnectionState<T>>,
    deadline: Instant,
    timer: tokio::task::JoinHandle<()>,
}

impl<T: MessageTransport + 'static> PendingConnection<T> {
    pub(crate) fn new(transport: T, permit: OwnedSemaphorePermit, deadline: Instant) -> Self {
        let state = Arc::new(PendingConnectionState {
            transport: Arc::new(transport),
            lease: AdmissionLease {
                status: AtomicU8::new(0),
                permit: Mutex::new(Some(permit)),
            },
        });
        let timer_state = Arc::clone(&state);
        let timer = tokio::spawn(async move {
            tokio::time::sleep_until(deadline).await;
            if timer_state
                .lease
                .status
                .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
                && timer_state.transport.close().await.is_ok()
            {
                timer_state.lease.release();
            }
            // A failed close retains admission with native ownership. The
            // final owner destroys the transport before the lease drops.
        });
        Self {
            state,
            deadline,
            timer,
        }
    }

    /// Constructs a native admission fixture. It is unavailable to production
    /// composition and establishes no protocol authority.
    #[cfg(feature = "test-support")]
    pub fn test_fixture(transport: T, permit: OwnedSemaphorePermit, deadline: Instant) -> Self {
        Self::new(transport, permit, deadline)
    }

    fn check_deadline(&self) -> Result<(), TransportError> {
        if Instant::now() >= self.deadline || self.state.lease.status.load(Ordering::Acquire) == 2 {
            Err(TransportError::HandshakeExpired)
        } else {
            Ok(())
        }
    }

    /// Joins native deadline ownership and destroys a rejected connection before
    /// returning. A close failure remains a failure, not teardown evidence.
    pub async fn close_joinable(mut self) -> Result<(), TransportError> {
        self.state.lease.status.store(2, Ordering::Release);
        self.timer.abort();
        let _ = (&mut self.timer).await;
        let result = self.state.transport.close().await;
        // The timer no longer retains state. Field order drops native ownership
        // before releasing its permit, including when close returned an error.
        drop(self);
        result
    }

    /// Releases the admission slot after composition verifies the pinned Server and Paired Device.
    ///
    /// This operation does not verify proofs. Calling it before successful protocol
    /// authentication is a composition error; local account eligibility alone is insufficient.
    pub fn finish_authentication(self) -> Result<Arc<T>, TransportError> {
        self.finish_authentication_joinable()
            .map_err(|_| TransportError::HandshakeExpired)
    }

    /// Finalizes readiness without losing failed connection ownership. On a
    /// deadline race the caller must await `close` before releasing its own
    /// admission. Native admission remains retained until transport teardown.
    pub fn finish_authentication_joinable(self) -> Result<Arc<T>, Self> {
        if self.check_deadline().is_err()
            || self
                .state
                .lease
                .status
                .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return Err(self);
        }
        self.state.lease.release();
        Ok(Arc::clone(&self.state.transport))
    }
}

impl<T: MessageTransport + 'static> Drop for PendingConnection<T> {
    fn drop(&mut self) {
        self.timer.abort();
    }
}

impl<T: MessageTransport + 'static> MessageTransport for PendingConnection<T> {
    fn write_progress(&self) -> Option<watch::Receiver<Instant>> {
        self.state.transport.write_progress()
    }
    fn peer_facts(&self) -> PeerFacts {
        self.state.transport.peer_facts()
    }

    fn send<'a>(&'a self, message: &'a [u8]) -> TransportFuture<'a, ()> {
        Box::pin(async move {
            self.check_deadline()?;
            tokio::time::timeout_at(self.deadline, self.state.transport.send(message))
                .await
                .map_err(|_| TransportError::HandshakeExpired)?
        })
    }

    fn recv(&self) -> TransportFuture<'_, Option<Vec<u8>>> {
        Box::pin(async move {
            self.check_deadline()?;
            tokio::time::timeout_at(self.deadline, self.state.transport.recv())
                .await
                .map_err(|_| TransportError::HandshakeExpired)?
        })
    }

    fn close(&self) -> TransportFuture<'_, ()> {
        Box::pin(async move {
            self.state.lease.status.store(2, Ordering::Release);
            let result = self.state.transport.close().await;
            if result.is_ok() {
                self.state.lease.release();
            }
            result
        })
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests {
    use super::*;
    use crate::ipc::{IpcListener, IpcTransport, PrivateIpcDirectory};
    use std::os::unix::fs::PermissionsExt;
    use tokio::sync::Semaphore;

    struct ClosingTransport {
        native: Arc<IpcTransport>,
        entered: Arc<Semaphore>,
        close: Arc<Semaphore>,
    }
    impl MessageTransport for ClosingTransport {
        fn peer_facts(&self) -> PeerFacts {
            self.native.peer_facts()
        }
        fn send<'a>(&'a self, message: &'a [u8]) -> TransportFuture<'a, ()> {
            self.native.send(message)
        }
        fn recv(&self) -> TransportFuture<'_, Option<Vec<u8>>> {
            self.native.recv()
        }
        fn close(&self) -> TransportFuture<'_, ()> {
            Box::pin(async {
                self.entered.add_permits(1);
                self.close.acquire().await.unwrap().forget();
                self.native.close().await
            })
        }
    }

    #[tokio::test(start_paused = true)]
    async fn readiness_deadline_race_never_releases_admission_before_native_close() {
        let temporary = tempfile::Builder::new()
            .prefix("st-race-")
            .tempdir_in("/private/tmp")
            .or_else(|_| {
                tempfile::Builder::new()
                    .prefix("st-race-")
                    .tempdir_in("/tmp")
            })
            .unwrap();
        std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let endpoint = PrivateIpcDirectory::open(temporary.path())
            .unwrap()
            .endpoint("s")
            .unwrap();
        let listener = IpcListener::bind(
            endpoint.clone(),
            MessageLimit::envelope(),
            AdmissionLimits::new(1, Duration::from_secs(5)).unwrap(),
        )
        .unwrap();
        let client = IpcTransport::connect(&endpoint, MessageLimit::envelope())
            .await
            .unwrap();
        let native = listener
            .accept()
            .await
            .unwrap()
            .finish_authentication()
            .unwrap();
        let slots = Arc::new(Semaphore::new(1));
        let entered = Arc::new(Semaphore::new(0));
        let close = Arc::new(Semaphore::new(0));
        let deadline = Instant::now() + Duration::from_secs(5);
        let pending = PendingConnection::new(
            ClosingTransport {
                native,
                entered: entered.clone(),
                close: close.clone(),
            },
            slots.clone().acquire_owned().await.unwrap(),
            deadline,
        );
        pending.send(&[1]).await.unwrap();
        assert_eq!(client.recv().await.unwrap(), Some(vec![1]));
        tokio::time::advance(Duration::from_secs(5)).await;
        let pending = match pending.finish_authentication_joinable() {
            Ok(_) => panic!("expired readiness must retain teardown ownership"),
            Err(pending) => pending,
        };
        let task = tokio::spawn(async move { pending.close().await });
        entered.acquire().await.unwrap().forget();
        assert_eq!(slots.available_permits(), 0);
        close.add_permits(2);
        task.await.unwrap().unwrap();
        assert_eq!(slots.available_permits(), 1);
        assert!(client.recv().await.unwrap().is_none());
        // One expired peer cannot permanently close either admission pool.
        let client = IpcTransport::connect(&endpoint, MessageLimit::envelope())
            .await
            .unwrap();
        let native = listener
            .accept()
            .await
            .unwrap()
            .finish_authentication()
            .unwrap();
        native.send(&[2]).await.unwrap();
        assert_eq!(client.recv().await.unwrap(), Some(vec![2]));
        native.close().await.unwrap();
        listener.close().unwrap();
    }
}

#[cfg(test)]
mod progress_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test(start_paused = true)]
    async fn only_successful_byte_writes_advance_native_progress() {
        let (writer, mut reader) = tokio::io::duplex(1);
        let mut writer = ProgressIo::new(writer);
        let mut progress = writer.progress();
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(writer.write(&[]).await.unwrap(), 0);
        assert!(!progress.has_changed().unwrap());
        writer.write_all(&[1]).await.unwrap();
        assert!(progress.has_changed().unwrap());
        assert_eq!(*progress.borrow_and_update(), Instant::now());

        tokio::time::advance(Duration::from_secs(1)).await;
        writer.flush().await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(1), writer.write(&[2]))
                .await
                .is_err()
        );
        assert!(!progress.has_changed().unwrap());
        assert_eq!(reader.read_u8().await.unwrap(), 1);
        assert!(!progress.has_changed().unwrap());

        let written = writer
            .write_vectored(&[std::io::IoSlice::new(&[2])])
            .await
            .unwrap();
        assert_eq!(written, 1);
        assert!(progress.has_changed().unwrap());
        assert_eq!(*progress.borrow_and_update(), Instant::now());
    }
}
