//! Authentication and retained connection teardown. Native account facts grant
//! eligibility only. Every route uses the protocol's exact signed handshake.

use crate::{
    adapters::transport::{check_peer, protocol_failure, transport_failure},
    api::{AuthenticatedSession, ServerHandle, VerifiedPrincipal},
    contracts::*,
    failure::failure,
};
use spaceterm_core::{
    authority::{ConnectionGeneration, ServerEpoch},
    identity::{ClientId, ServerId},
};
use spaceterm_host::transport::{MessageTransport, PendingConnection};
use spaceterm_protocol::{
    authentication::{Established, HandshakeConfig, ServerHandshake, SigningKey},
    connection::RestrictedConnection,
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, oneshot, watch};

pub const MAX_UNAUTHENTICATED_CONNECTIONS: usize = 16;
pub const MAX_AUTHENTICATED_CONNECTIONS: usize = 32;
pub const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(5);

/// Registration is serialized with revocation by the authoritative actor.
/// A test may supply an in-crate fake of this port, never a production credential.
pub trait ConnectionAuthorityPort: Send + Sync {
    /// Success installs the exact binding; rejection installs nothing. The caller
    /// retains a submitted registration future until its result settles.
    fn register(&self, principal: VerifiedPrincipal)
    -> ServerFuture<'_, Result<(), ServerFailure>>;
    /// Disconnect only this binding. An absent or superseded generation is
    /// already inert and succeeds without touching the replacement binding.
    fn disconnect(
        &self,
        principal: VerifiedPrincipal,
    ) -> ServerFuture<'_, Result<(), ServerFailure>>;
}
impl ConnectionAuthorityPort for ServerHandle {
    fn register(
        &self,
        principal: VerifiedPrincipal,
    ) -> ServerFuture<'_, Result<(), ServerFailure>> {
        Box::pin(async move { ServerHandle::register(self, principal).await.map(|_| ()) })
    }
    fn disconnect(
        &self,
        principal: VerifiedPrincipal,
    ) -> ServerFuture<'_, Result<(), ServerFailure>> {
        Box::pin(async move {
            match ServerHandle::disconnect(self, principal).await {
                // The actor compares the complete principal before touching any
                // binding. Absent and superseded generations are already inert.
                Err(error)
                    if matches!(
                        error.class,
                        ServerFailureClass::Authentication | ServerFailureClass::StaleAuthority
                    ) =>
                {
                    Ok(())
                }
                result => result,
            }
        })
    }
}

pub struct AuthenticationConfig {
    pub server: ServerId,
    pub epoch: ServerEpoch,
    pub owning_uid: u32,
    pub handshake: HandshakeConfig,
    pub credentials: Arc<dyn ServerCredentialPort>,
    pub pairing: Arc<dyn PairingAuthorizationPort>,
    pub authority: Arc<dyn ConnectionAuthorityPort>,
    pub unauthenticated_limit: usize,
    pub authenticated_limit: usize,
    pub deadline: Duration,
}

/// Acquire before native accept, including WSS TLS. Hold until readiness or
/// transport teardown. Dropping a rejected native PendingConnection first keeps
/// the host's own ordered admission lease intact.
pub struct UnauthenticatedPermit(OwnedSemaphorePermit);

struct CloseSignal {
    close: watch::Sender<bool>,
    closed: watch::Receiver<Option<Result<(), ServerFailure>>>,
}
impl CloseSignal {
    fn request(&self) {
        self.close.send_replace(true);
    }
    async fn wait(&self) -> Result<(), ServerFailure> {
        let mut closed = self.closed.clone();
        let result = closed
            .wait_for(Option::is_some)
            .await
            .map_err(|_| failure(ServerFailureClass::CleanupIncomplete))?;
        (*result).ok_or_else(|| failure(ServerFailureClass::CleanupIncomplete))?
    }
}
struct ActiveConnection {
    generation: u64,
    client: Option<ClientId>,
    signal: Arc<CloseSignal>,
}
struct HandshakeRegistration {
    signal: Arc<CloseSignal>,
    closed: watch::Sender<Option<Result<(), ServerFailure>>>,
}

enum AuthorityRegistration {
    NotSubmitted,
    InFlight(tokio::task::JoinHandle<Result<(), ServerFailure>>),
    Installed,
    Rejected,
    SettlementUnknown,
}
impl AuthorityRegistration {
    async fn settle(&mut self) -> Result<(), ServerFailure> {
        let Self::InFlight(task) = self else {
            return Err(failure(ServerFailureClass::RecoveryRequired));
        };
        match task.await {
            Ok(Ok(())) => {
                *self = Self::Installed;
                Ok(())
            }
            Ok(Err(error)) => {
                *self = Self::Rejected;
                Err(error)
            }
            Err(_) => {
                *self = Self::SettlementUnknown;
                Err(failure(ServerFailureClass::CleanupIncomplete))
            }
        }
    }
    async fn cleanup(
        &mut self,
        authority: &dyn ConnectionAuthorityPort,
        principal: Option<VerifiedPrincipal>,
    ) -> Result<(), ServerFailure> {
        if matches!(self, Self::InFlight(_)) {
            // Lifecycle disconnect has a separate actor queue. Await the exact
            // registration result so it cannot install after disconnect returns.
            let _ = self.settle().await;
        }
        match (self, principal) {
            (Self::Installed, Some(principal)) => authority.disconnect(principal).await,
            (Self::SettlementUnknown, _) => Err(failure(ServerFailureClass::CleanupIncomplete)),
            _ => Ok(()),
        }
    }
}

struct Shared {
    config: AuthenticationConfig,
    enrollment:
        Option<Arc<dyn spaceterm_protocol::pairing_adapter::EnrollmentLookup + Send + Sync>>,
    unauthenticated: Arc<Semaphore>,
    authenticated: Arc<Semaphore>,
    generations: AtomicU64,
    active: Mutex<BTreeMap<u64, ActiveConnection>>,
    ordinary: Mutex<BTreeMap<ClientId, u64>>,
    closed: watch::Sender<bool>,
    teardown_failed: AtomicBool,
    handshakes: AtomicUsize,
    handshakes_finished: tokio::sync::Notify,
}

#[derive(Clone)]
pub struct Authenticator(Arc<Shared>);
impl Authenticator {
    pub fn new(config: AuthenticationConfig) -> Result<Self, ServerFailure> {
        Self::construct(config, None)
    }
    /// Enrollment has a separate pinned transcript and never registers an
    /// ordinary principal, even when its prospective Client is already known.
    pub fn new_with_enrollment(
        config: AuthenticationConfig,
        enrollment: Arc<dyn spaceterm_protocol::pairing_adapter::EnrollmentLookup + Send + Sync>,
    ) -> Result<Self, ServerFailure> {
        Self::construct(config, Some(enrollment))
    }
    fn construct(
        config: AuthenticationConfig,
        enrollment: Option<
            Arc<dyn spaceterm_protocol::pairing_adapter::EnrollmentLookup + Send + Sync>,
        >,
    ) -> Result<Self, ServerFailure> {
        if config.server.bytes() == [0; 16]
            || config.epoch.get() == 0
            || config.unauthenticated_limit == 0
            || config.unauthenticated_limit > MAX_UNAUTHENTICATED_CONNECTIONS
            || config.authenticated_limit == 0
            || config.authenticated_limit > MAX_AUTHENTICATED_CONNECTIONS
            || config.deadline.is_zero()
            || config.deadline > HANDSHAKE_DEADLINE
        {
            return Err(failure(ServerFailureClass::Capacity));
        }
        config
            .handshake
            .limits
            .validate()
            .map_err(protocol_failure)?;
        Ok(Self(Arc::new(Shared {
            unauthenticated: Arc::new(Semaphore::new(config.unauthenticated_limit)),
            authenticated: Arc::new(Semaphore::new(config.authenticated_limit)),
            generations: AtomicU64::new(0),
            active: Mutex::new(BTreeMap::new()),
            ordinary: Mutex::new(BTreeMap::new()),
            closed: watch::channel(false).0,
            teardown_failed: AtomicBool::new(false),
            handshakes: AtomicUsize::new(0),
            handshakes_finished: tokio::sync::Notify::new(),
            config,
            enrollment,
        })))
    }
    pub fn try_admit(&self) -> Result<UnauthenticatedPermit, ServerFailure> {
        if *self.0.closed.borrow() {
            return Err(failure(ServerFailureClass::Authentication));
        }
        self.0
            .unauthenticated
            .clone()
            .try_acquire_owned()
            .map(UnauthenticatedPermit)
            .map_err(|_| failure(ServerFailureClass::Capacity))
    }
    pub async fn admit(&self) -> Result<UnauthenticatedPermit, ServerFailure> {
        self.0
            .unauthenticated
            .clone()
            .acquire_owned()
            .await
            .map(UnauthenticatedPermit)
            .map_err(|_| failure(ServerFailureClass::Capacity))
    }
    fn next_generation(&self) -> Result<u64, ServerFailure> {
        self.0
            .generations
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                value.checked_add(1)
            })
            .map(|value| value + 1)
            .map_err(|_| failure(ServerFailureClass::Exhausted))
    }

    /// A retained task owns admission through cancellation and transport close.
    /// Dropping the caller future cannot release its permit ahead of its socket.
    pub async fn authenticate<T: MessageTransport + 'static>(
        &self,
        pending: PendingConnection<T>,
        permit: UnauthenticatedPermit,
    ) -> Result<AuthenticatedPeer, ServerFailure> {
        self.authenticate_until(pending, permit, self.0.closed.subscribe())
            .await
    }

    pub async fn authenticate_until<T: MessageTransport + 'static>(
        &self,
        pending: PendingConnection<T>,
        permit: UnauthenticatedPermit,
        stopping: watch::Receiver<bool>,
    ) -> Result<AuthenticatedPeer, ServerFailure> {
        let (mut reply, result) = oneshot::channel();
        self.0.handshakes.fetch_add(1, Ordering::AcqRel);
        let owner = self.clone();
        tokio::spawn(async move {
            let result = owner
                .authenticate_owned(pending, permit, &mut reply, stopping)
                .await;
            let _ = reply.send(result);
            owner.0.handshakes.fetch_sub(1, Ordering::AcqRel);
            owner.0.handshakes_finished.notify_waiters();
        });
        result
            .await
            .map_err(|_| failure(ServerFailureClass::CleanupIncomplete))?
    }

    async fn close_pending<T: MessageTransport + 'static>(
        &self,
        pending: PendingConnection<T>,
    ) -> Result<(), ServerFailure> {
        let result = pending.close_joinable().await.map_err(transport_failure);
        if result.is_err() {
            self.0.teardown_failed.store(true, Ordering::Release);
        }
        result
    }

    async fn authenticate_owned<T: MessageTransport + 'static>(
        &self,
        pending: PendingConnection<T>,
        permit: UnauthenticatedPermit,
        reply: &mut oneshot::Sender<Result<AuthenticatedPeer, ServerFailure>>,
        mut stopping: watch::Receiver<bool>,
    ) -> Result<AuthenticatedPeer, ServerFailure> {
        let UnauthenticatedPermit(permit) = permit;
        if !Arc::ptr_eq(permit.semaphore(), &self.0.unauthenticated) {
            let _ = self.close_pending(pending).await;
            return Err(failure(ServerFailureClass::Authentication));
        }
        let deadline = tokio::time::Instant::now() + self.0.config.deadline;
        let mut shutdown = self.0.closed.subscribe();
        let result = tokio::select! {
            biased;
            _ = reply.closed() => Err(failure(ServerFailureClass::Authentication)),
            _ = stopping.wait_for(|value| *value) => Err(failure(ServerFailureClass::Authentication)),
            _ = shutdown.wait_for(|value| *value) => Err(failure(ServerFailureClass::Authentication)),
            result = tokio::time::timeout_at(deadline, self.handshake(&pending)) => result.unwrap_or_else(|_| Err(failure(ServerFailureClass::Authentication))),
        };
        let (connection, ready, generation) = match result {
            Ok(value) => value,
            Err(error) => {
                let _ = self.close_pending(pending).await;
                drop(permit);
                return Err(error);
            }
        };
        let principal = match &connection {
            PeerConnection::Ordinary(session) => Some(session.principal),
            PeerConnection::Restricted(_) | PeerConnection::Enrollment(_) => None,
        };
        // Install verified, in-progress identities before actor registration.
        // A durable revocation must also cancel readiness and join this teardown.
        let registration = match self.track_handshake(principal, generation) {
            Ok(value) => value,
            Err(error) => {
                let _ = self.close_pending(pending).await;
                drop(permit);
                return Err(error);
            }
        };
        let mut revoked = registration.signal.close.subscribe();
        let mut authenticated_slot = None;
        let mut authority_registration = AuthorityRegistration::NotSubmitted;
        let registered = async {
            if let Some(principal) = principal {
                let old = {
                    let ordinary = self
                        .0
                        .ordinary
                        .lock()
                        .map_err(|_| failure(ServerFailureClass::RecoveryRequired))?;
                    let active = self
                        .0
                        .active
                        .lock()
                        .map_err(|_| failure(ServerFailureClass::RecoveryRequired))?;
                    ordinary
                        .get(&principal.client())
                        .and_then(|id| active.get(id))
                        .map(|entry| entry.signal.clone())
                };
                if let Some(old) = old {
                    old.request();
                    old.wait().await?;
                }
            }
            authenticated_slot = Some(
                self.0
                    .authenticated
                    .clone()
                    .try_acquire_owned()
                    .map_err(|_| failure(ServerFailureClass::Capacity))?,
            );
            if let Some(principal) = principal {
                let authority = self.0.config.authority.clone();
                authority_registration =
                    AuthorityRegistration::InFlight(tokio::spawn(async move {
                        authority.register(principal).await
                    }));
                authority_registration.settle().await?;
            }
            pending.send(&ready).await.map_err(transport_failure)?;
            Ok(())
        };
        let result = tokio::select! {
            biased;
            _ = reply.closed() => Err(failure(ServerFailureClass::Authentication)),
            _ = stopping.wait_for(|value| *value) => Err(failure(ServerFailureClass::Authentication)),
            _ = shutdown.wait_for(|value| *value) => Err(failure(ServerFailureClass::Authentication)),
            _ = revoked.wait_for(|value| *value) => Err(failure(ServerFailureClass::Authentication)),
            result = tokio::time::timeout_at(deadline, registered) => result.unwrap_or_else(|_| Err(failure(ServerFailureClass::Authentication))),
        };
        match result {
            Ok(()) => {}
            Err(error) => {
                let cleanup = self.close_pending(pending).await;
                let cleanup = cleanup.and(
                    authority_registration
                        .cleanup(self.0.config.authority.as_ref(), principal)
                        .await,
                );
                drop(authenticated_slot);
                drop(permit);
                self.finish_registration(generation, registration, cleanup);
                return Err(error);
            }
        };
        let authenticated = authenticated_slot
            .take()
            .ok_or_else(|| failure(ServerFailureClass::RecoveryRequired))?;
        let transport = match pending.finish_authentication_joinable() {
            Ok(value) => value,
            Err(pending) => {
                let cleanup = self.close_pending(pending).await;
                let cleanup = cleanup.and(
                    authority_registration
                        .cleanup(self.0.config.authority.as_ref(), principal)
                        .await,
                );
                drop(authenticated);
                drop(permit);
                self.finish_registration(generation, registration, cleanup);
                return Err(failure(ServerFailureClass::Authentication));
            }
        };
        let transport: Arc<dyn MessageTransport> = transport;
        let peer = self
            .retain(
                connection,
                transport,
                generation,
                authenticated,
                registration,
            )
            .await?;
        drop(permit);
        Ok(peer)
    }

    async fn handshake<T: MessageTransport + 'static>(
        &self,
        pending: &PendingConnection<T>,
    ) -> Result<(PeerConnection, Vec<u8>, u64), ServerFailure> {
        check_peer(pending.peer_facts(), self.0.config.owning_uid)?;
        let hello = pending
            .recv()
            .await
            .map_err(transport_failure)?
            .ok_or_else(|| failure(ServerFailureClass::Authentication))?;
        let signer: Arc<dyn SigningKey> = self.0.config.credentials.clone();
        if spaceterm_protocol::pairing_adapter::is_enrollment_hello(&hello) {
            let lookup = self
                .0
                .enrollment
                .as_ref()
                .ok_or_else(|| failure(ServerFailureClass::Authentication))?;
            let (handshake, reply) = spaceterm_protocol::pairing_adapter::EnrollmentServer::accept(
                self.0.config.server.bytes(),
                self.0.config.handshake.majors.clone(),
                signer,
                lookup.as_ref(),
                &hello,
            )
            .map_err(protocol_failure)?;
            pending.send(&reply).await.map_err(transport_failure)?;
            let proof = pending
                .recv()
                .await
                .map_err(transport_failure)?
                .ok_or_else(|| failure(ServerFailureClass::Authentication))?;
            let (connection, ready) = handshake
                .receive_proof(lookup.as_ref(), &proof)
                .map_err(protocol_failure)?;
            return Ok((
                PeerConnection::Enrollment(Box::new(connection)),
                ready,
                self.next_generation()?,
            ));
        }
        let (handshake, reply) = ServerHandshake::accept(
            self.0.config.server.bytes(),
            self.0.config.epoch.get(),
            self.0.config.handshake.clone(),
            signer,
            self.0.config.pairing.as_ref(),
            &hello,
        )
        .map_err(protocol_failure)?;
        pending.send(&reply).await.map_err(transport_failure)?;
        let proof = pending
            .recv()
            .await
            .map_err(transport_failure)?
            .ok_or_else(|| failure(ServerFailureClass::Authentication))?;
        let (established, ready) = handshake
            .receive_device(self.0.config.pairing.as_ref(), &proof)
            .map_err(protocol_failure)?;
        let generation = self.next_generation()?;
        let connection = match established {
            Established::Ordinary(codec) => PeerConnection::Ordinary(AuthenticatedSession::new(
                codec,
                ConnectionGeneration::new(generation),
            )?),
            Established::Restricted(codec) => PeerConnection::Restricted(codec),
        };
        Ok((connection, ready, generation))
    }

    fn track_handshake(
        &self,
        principal: Option<VerifiedPrincipal>,
        generation: u64,
    ) -> Result<HandshakeRegistration, ServerFailure> {
        let (close, _) = watch::channel(false);
        let (closed, closed_receive) = watch::channel(None);
        let signal = Arc::new(CloseSignal {
            close,
            closed: closed_receive,
        });
        let mut active = self
            .0
            .active
            .lock()
            .map_err(|_| failure(ServerFailureClass::RecoveryRequired))?;
        if *self.0.closed.borrow() {
            return Err(failure(ServerFailureClass::Authentication));
        }
        active.insert(
            generation,
            ActiveConnection {
                generation,
                client: principal.map(|p| p.client()),
                signal: signal.clone(),
            },
        );
        Ok(HandshakeRegistration { signal, closed })
    }

    fn finish_registration(
        &self,
        generation: u64,
        registration: HandshakeRegistration,
        result: Result<(), ServerFailure>,
    ) {
        if result.is_err() {
            self.0.teardown_failed.store(true, Ordering::Release);
        }
        if let Ok(mut active) = self.0.active.lock() {
            active.remove(&generation);
        }
        registration.closed.send_replace(Some(result));
    }

    async fn retain(
        &self,
        connection: PeerConnection,
        transport: Arc<dyn MessageTransport>,
        generation: u64,
        permit: OwnedSemaphorePermit,
        registration: HandshakeRegistration,
    ) -> Result<AuthenticatedPeer, ServerFailure> {
        let principal = match &connection {
            PeerConnection::Ordinary(session) => Some(session.principal),
            PeerConnection::Restricted(_) | PeerConnection::Enrollment(_) => None,
        };
        let signal = registration.signal.clone();
        let mut closing = signal.close.subscribe();
        let installed = (|| {
            let mut ordinary = self
                .0
                .ordinary
                .lock()
                .map_err(|_| failure(ServerFailureClass::RecoveryRequired))?;
            let active = self
                .0
                .active
                .lock()
                .map_err(|_| failure(ServerFailureClass::RecoveryRequired))?;
            if *self.0.closed.borrow()
                || !active.get(&generation).is_some_and(|entry| {
                    entry.generation == generation
                        && entry.client == principal.map(|p| p.client())
                        && Arc::ptr_eq(&entry.signal, &signal)
                        && !*entry.signal.close.borrow()
                })
            {
                return Err(failure(ServerFailureClass::Authentication));
            }
            if let Some(principal) = principal {
                if let Some(old) = ordinary
                    .get(&principal.client())
                    .and_then(|id| active.get(id))
                {
                    if old.generation >= generation {
                        return Err(failure(ServerFailureClass::StaleAuthority));
                    }
                    old.signal.request();
                }
                ordinary.insert(principal.client(), generation);
            }
            Ok(())
        })();
        if let Err(error) = installed {
            let mut cleanup = transport.close().await.map_err(transport_failure);
            if let Some(principal) = principal {
                cleanup = cleanup.and(self.0.config.authority.disconnect(principal).await);
            }
            drop(transport);
            drop(permit);
            self.finish_registration(generation, registration, cleanup);
            return Err(error);
        }
        let (owner, abandoned) = oneshot::channel::<()>();
        let retained_transport = transport.clone();
        let shared = self.0.clone();
        let join = tokio::spawn(async move {
            tokio::select! { _ = closing.wait_for(|value| *value) => {}, _ = abandoned => {} }
            if let Some(principal) = principal {
                let _ = shared.config.authority.disconnect(principal).await;
            }
            let transport_result = retained_transport.close().await.map_err(transport_failure);
            if transport_result.is_err() {
                shared.teardown_failed.store(true, Ordering::Release);
            }
            if let Ok(mut ordinary) = shared.ordinary.lock()
                && let Some(principal) = principal
                && ordinary.get(&principal.client()) == Some(&generation)
            {
                ordinary.remove(&principal.client());
            }
            if let Ok(mut active) = shared.active.lock() {
                active.remove(&generation);
            }
            drop(retained_transport);
            drop(permit);
            registration.closed.send_replace(Some(transport_result));
            transport_result
        });
        Ok(AuthenticatedPeer {
            connection,
            transport,
            closing: signal.close.subscribe(),
            owner: Some(owner),
            join: Some(join),
        })
    }

    /// Recheck before dispatch and final egress. Neither a restricted session nor
    /// a superseded connection can obtain current ordinary access.
    pub fn check_current(&self, principal: VerifiedPrincipal) -> Result<(), ServerFailure> {
        if *self.0.closed.borrow()
            || principal.server() != self.0.config.server
            || principal.epoch() != self.0.config.epoch
        {
            return Err(failure(ServerFailureClass::Authentication));
        }
        let eligible = {
            let ordinary = self
                .0
                .ordinary
                .lock()
                .map_err(|_| failure(ServerFailureClass::RecoveryRequired))?;
            let active = self
                .0
                .active
                .lock()
                .map_err(|_| failure(ServerFailureClass::RecoveryRequired))?;
            ordinary
                .get(&principal.client())
                .and_then(|id| active.get(id))
                .is_some_and(|entry| {
                    entry.generation == principal.binding().generation.get()
                        && !*entry.signal.close.borrow()
                })
        };
        if !eligible {
            return Err(failure(ServerFailureClass::StaleAuthority));
        }
        let p = principal;
        let local = |value: u64| {
            let mut bytes = [0; 16];
            bytes[8..].copy_from_slice(&value.to_be_bytes());
            bytes
        };
        let expected = spaceterm_protocol::authentication::Principal {
            server: p.server().bytes(),
            device: p.device().bytes(),
            pairing: local(p.pairing().local()),
            client: local(p.client().local()),
        };
        let record = self
            .0
            .config
            .pairing
            .lookup(expected.server, expected.pairing, expected.client)
            .map_err(protocol_failure)?;
        if !record.active || record.principal != expected {
            return Err(failure(ServerFailureClass::Authentication));
        }
        Ok(())
    }

    pub fn request_close_clients(&self, clients: &[ClientId]) -> Result<(), ServerFailure> {
        let active = self
            .0
            .active
            .lock()
            .map_err(|_| failure(ServerFailureClass::RecoveryRequired))?;
        for entry in active.values() {
            if entry.client.is_some_and(|client| clients.contains(&client)) {
                entry.signal.request();
            }
        }
        Ok(())
    }

    /// Called by trusted composition after durable revocation. Restricted retry
    /// channels remain separate and do not restore ordinary registrations.
    pub async fn close_clients(&self, clients: &[ClientId]) -> Result<(), ServerFailure> {
        let signals = {
            let active = self
                .0
                .active
                .lock()
                .map_err(|_| failure(ServerFailureClass::RecoveryRequired))?;
            active
                .values()
                .filter(|entry| entry.client.is_some_and(|client| clients.contains(&client)))
                .map(|entry| entry.signal.clone())
                .collect::<Vec<_>>()
        };
        for signal in &signals {
            signal.request();
        }
        for signal in signals {
            signal.wait().await?;
        }
        Ok(())
    }
    pub async fn close(&self) -> Result<(), ServerFailure> {
        self.0.closed.send_replace(true);
        self.0.unauthenticated.close();
        self.0.authenticated.close();
        loop {
            let notified = self.0.handshakes_finished.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.0.handshakes.load(Ordering::Acquire) == 0 {
                break;
            }
            notified.await;
        }
        let signals = self
            .0
            .active
            .lock()
            .map_err(|_| failure(ServerFailureClass::RecoveryRequired))?
            .values()
            .map(|entry| entry.signal.clone())
            .collect::<Vec<_>>();
        for signal in &signals {
            signal.request();
        }
        for signal in signals {
            signal.wait().await?;
        }
        if self.0.teardown_failed.load(Ordering::Acquire) {
            return Err(failure(ServerFailureClass::CleanupIncomplete));
        }
        Ok(())
    }
}

pub enum PeerConnection {
    Ordinary(AuthenticatedSession),
    Restricted(RestrictedConnection),
    Enrollment(Box<spaceterm_protocol::pairing_adapter::EnrollmentConnection>),
}
/// Dropping presentation/protocol ownership asks the retained transport supervisor
/// to close it. Admission releases only after that teardown, never before it.
pub struct AuthenticatedPeer {
    pub connection: PeerConnection,
    pub transport: Arc<dyn MessageTransport>,
    pub closing: watch::Receiver<bool>,
    owner: Option<oneshot::Sender<()>>,
    join: Option<tokio::task::JoinHandle<Result<(), ServerFailure>>>,
}
impl AuthenticatedPeer {
    pub async fn close(mut self) -> Result<(), ServerFailure> {
        self.owner.take();
        if let Some(join) = self.join.take() {
            join.await
                .map_err(|_| failure(ServerFailureClass::CleanupIncomplete))?
        } else {
            Ok(())
        }
    }
}
