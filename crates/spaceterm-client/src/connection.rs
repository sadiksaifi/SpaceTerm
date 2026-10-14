//! One cancellable authenticated worker per saved server. Reachability grants no
//! authorization, and interruption never preserves an ephemeral outbound queue.

pub(crate) mod queries;
pub(crate) mod writer;

use crate::{
    api::{
        CompatibilityExplanation, ConnectedInfo, ConnectingPhase, ConnectionState,
        PairedServerBinding, RetryState,
    },
    failure::{
        ClientFailure, ClientFailureClass, RetryDisposition, exhausted, invalid_peer,
        resource_limit,
    },
    limits::{
        DIAL_DEADLINE, HANDSHAKE_DEADLINE, MAX_RESYNC_FAILURES, RECONCILIATION_DEADLINE,
        RETRY_MAXIMUM, RETRY_MINIMUM,
    },
    ports::{
        ConnectedRoute, ConnectionInvalidation, ConnectionPurpose, DiscoveryState,
        ForegroundCancellation, MessageSender, OrganizationPublication,
    },
    protocol_adapter::{
        ProtocolSession, confirmed_stopped, is_organization, is_resync, organization_query,
        reconciliation, reconciliation_binding,
    },
    registry::{EgressBudget, EgressReservation, Shared, cancelled, same_connection},
    replica::{Replica, ReplicaUpdate, ResyncReason},
};
use sha2::{Digest, Sha256};
use spaceterm_core::authority::ServerEpoch;
use spaceterm_protocol::codec::Payload;
use std::{sync::Arc, time::Duration};
use tokio::{
    sync::{mpsc, oneshot, watch},
    time::{Instant, timeout, timeout_at},
};

pub(crate) struct ConnectRequest {
    pub retry: bool,
    pub completion: oneshot::Sender<Result<(), ClientFailure>>,
}
pub(crate) struct DispatchRequest {
    pub captured: ConnectedInfo,
    pub query: bool,
    pub message: Payload,
    pub completion: oneshot::Sender<Result<(), ClientFailure>>,
    pub _reservation: EgressReservation,
}

pub(crate) struct WorkerChannels {
    pub requests: mpsc::Receiver<ConnectRequest>,
    pub outgoing: mpsc::Receiver<DispatchRequest>,
    pub shutdown: watch::Receiver<bool>,
    pub egress: Arc<EgressBudget>,
    pub queries: Arc<queries::QueryHub>,
}

struct AttemptResources {
    foreground: Option<ForegroundCancellation>,
    queries: Arc<queries::QueryHub>,
    egress: Arc<EgressBudget>,
    writer_done: Option<watch::Receiver<bool>>,
    opening: Option<tokio::task::JoinHandle<Result<ConnectedRoute, ClientFailure>>>,
    sender: Option<Arc<dyn MessageSender>>,
    slot: Option<tokio::sync::OwnedSemaphorePermit>,
}
impl AttemptResources {
    fn new(
        egress: Arc<EgressBudget>,
        queries: Arc<queries::QueryHub>,
        foreground_retry: bool,
    ) -> Self {
        Self {
            foreground: foreground_retry.then(ForegroundCancellation::new),
            queries,
            egress,
            writer_done: None,
            opening: None,
            sender: None,
            slot: None,
        }
    }
    async fn close(&mut self) -> Result<(), ClientFailure> {
        if let Some(foreground) = self.foreground.take() {
            foreground.cancel();
        }
        // Revoke prompt authority before waiting for the native open. Keep its
        // owned task so shutdown and replacement Retry still join exact cleanup.
        if let Some(opening) = self.opening.take()
            && let Ok(Ok(route)) = opening.await
        {
            self.sender = Some(Arc::clone(&route.sender));
        }
        if let Some(mut done) = self.writer_done.take() {
            while !*done.borrow_and_update() {
                done.changed().await.map_err(|_| cancelled())?;
            }
        }
        let result = if let Some(sender) = self.sender.take() {
            sender.abort();
            sender.close().await
        } else {
            Ok(())
        };
        self.slot.take();
        result
    }
}

enum AttemptEnd {
    Finished(Result<(), ClientFailure>),
    Restart,
    Shutdown,
}

pub(crate) async fn run(
    shared: Arc<Shared>,
    binding: PairedServerBinding,
    state: watch::Sender<ConnectionState>,
    mut replica: Replica,
    channels: WorkerChannels,
) -> Result<(), ClientFailure> {
    let WorkerChannels {
        mut requests,
        mut outgoing,
        mut shutdown,
        egress,
        queries,
    } = channels;
    let mut retry_at: Option<Instant> = None;
    let mut attempt = 0u64;
    let mut failures = 0u32;
    let mut foreground_retry = false;
    loop {
        if *shutdown.borrow() {
            break;
        }
        tokio::select! {
            biased;
            _ = shutdown.changed() => break,
            request = requests.recv() => match request {
                Some(request) => {
                    foreground_retry = request.retry;
                    let _ = request.completion.send(Ok(()));
                }
                None => break,
            },
            request = outgoing.recv() => {
                if let Some(request) = request { let _ = request.completion.send(Err(stale())); }
                continue;
            },
            _ = wait_retry(retry_at) => {},
        }
        attempt = match attempt.checked_add(1) {
            Some(value) => value,
            None => {
                offline(&state, exhausted(), failures, None);
                continue;
            }
        };
        queries.invalidate();
        shared
            .services
            .delegate
            .invalidate(&binding, ConnectionInvalidation::Reconciliation);
        replica.invalidate(ResyncReason::ConnectionLost);
        reject_outgoing(&mut outgoing);
        let mut resources = AttemptResources::new(
            Arc::clone(&egress),
            Arc::clone(&queries),
            std::mem::take(&mut foreground_retry),
        );
        let end = {
            let pending = run_attempt(
                &shared,
                &binding,
                &state,
                &mut replica,
                &mut outgoing,
                attempt,
                &mut resources,
            );
            tokio::pin!(pending);
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown.changed() => break AttemptEnd::Shutdown,
                    request = requests.recv() => match request {
                        Some(request) => {
                            let connected = matches!(*state.borrow(), ConnectionState::Connected { .. });
                            let _ = request.completion.send(Ok(()));
                            if request.retry || !connected {
                                foreground_retry = request.retry;
                                break AttemptEnd::Restart;
                            }
                        }
                        None => break AttemptEnd::Shutdown,
                    },
                    result = &mut pending => break AttemptEnd::Finished(result),
                }
            }
        };
        // Dropping the exact attempt cancels read/send/journal callbacks. Close
        // revokes its prompts and drains its owned dial before publishing a replacement.
        let reason = match &end {
            AttemptEnd::Finished(Ok(())) => ConnectionInvalidation::Stopped,
            AttemptEnd::Shutdown => ConnectionInvalidation::Shutdown,
            _ => ConnectionInvalidation::ConnectionLost,
        };
        queries.invalidate();
        shared.services.delegate.invalidate(&binding, reason);
        replica.invalidate(ResyncReason::ConnectionLost);
        reject_outgoing(&mut outgoing);
        if let Err(error) = resources.close().await {
            offline(&state, error, failures, None);
            return Err(error);
        }
        match end {
            AttemptEnd::Shutdown => break,
            AttemptEnd::Restart => {
                retry_at = Some(Instant::now());
                failures = 0;
            }
            AttemptEnd::Finished(Ok(())) => {
                state.send_replace(ConnectionState::Stopped);
                retry_at = None;
                failures = 0;
            }
            AttemptEnd::Finished(Err(error)) => {
                failures = failures.saturating_add(1);
                let delay = (error.retry() == RetryDisposition::Automatic)
                    .then(|| retry_delay(&binding, failures));
                retry_at = delay.map(|delay| Instant::now() + delay);
                offline(&state, error, failures, delay);
            }
        }
    }
    shared
        .services
        .delegate
        .invalidate(&binding, ConnectionInvalidation::Shutdown);
    replica.invalidate(ResyncReason::ConnectionLost);
    reject_outgoing(&mut outgoing);
    offline(&state, cancelled(), 0, None);
    Ok(())
}

async fn wait_retry(at: Option<Instant>) {
    if let Some(at) = at {
        tokio::time::sleep_until(at).await;
    } else {
        std::future::pending::<()>().await;
    }
}
fn stale() -> ClientFailure {
    ClientFailure::new(ClientFailureClass::StaleAuthority)
}
fn reject_outgoing(outgoing: &mut mpsc::Receiver<DispatchRequest>) {
    while let Ok(request) = outgoing.try_recv() {
        let _ = request.completion.send(Err(stale()));
    }
}
fn offline(
    state: &watch::Sender<ConnectionState>,
    error: ClientFailure,
    failures: u32,
    delay: Option<Duration>,
) {
    state.send_replace(if error.class() == ClientFailureClass::Incompatible {
        ConnectionState::Incompatible {
            explanation: CompatibilityExplanation::NoCommonMajor,
        }
    } else if error.class() == ClientFailureClass::MissingCapability {
        ConnectionState::Incompatible {
            explanation: CompatibilityExplanation::CanonicalDomainRequired,
        }
    } else {
        ConnectionState::Offline {
            failure: error,
            retry: RetryState { failures, delay },
        }
    });
}
fn retry_delay(binding: &PairedServerBinding, failures: u32) -> Duration {
    let multiplier = 1u64 << failures.saturating_sub(1).min(7);
    let base =
        (RETRY_MINIMUM.as_millis() as u64 * multiplier).min(RETRY_MAXIMUM.as_millis() as u64);
    let mut hash = Sha256::new();
    hash.update(binding.target().server().bytes());
    hash.update(binding.target().client().local().to_be_bytes());
    hash.update(failures.to_be_bytes());
    let jitter = u64::from(hash.finalize()[0]) * base / 1024;
    Duration::from_millis((base + jitter).min(RETRY_MAXIMUM.as_millis() as u64))
}

async fn run_attempt(
    shared: &Shared,
    binding: &PairedServerBinding,
    state: &watch::Sender<ConnectionState>,
    replica: &mut Replica,
    outgoing: &mut mpsc::Receiver<DispatchRequest>,
    attempt: u64,
    resources: &mut AttemptResources,
) -> Result<(), ClientFailure> {
    let services = &shared.services;
    let target = binding.target();
    let queries = Arc::clone(&resources.queries);
    state.send_replace(ConnectionState::Connecting {
        phase: ConnectingPhase::Dialing,
    });
    let allowed = timeout(DIAL_DEADLINE, async {
        if !services.journal.ordinary_use_allowed(binding).await? {
            return Err(ClientFailure::new(ClientFailureClass::AuthorizationDenied));
        }
        Ok(!matches!(&services.discovery, Some(discovery) if discovery.status(target).await? == DiscoveryState::Stopped))
    }).await.map_err(|_| crate::protocol_adapter::unavailable())??;
    if !allowed {
        return Ok(());
    }
    resources.slot = Some(
        Arc::clone(&shared.slots)
            .try_acquire_owned()
            .map_err(|_| resource_limit())?,
    );
    let routes = Arc::clone(&services.routes);
    let owned_target = target.clone();
    let foreground = resources.foreground.clone();
    resources.opening = Some(tokio::spawn(async move {
        if let Some(cancellation) = foreground {
            routes.open_foreground(&owned_target, cancellation).await
        } else {
            routes
                .open(&owned_target, ConnectionPurpose::Ordinary)
                .await
        }
    }));
    // The deadline bounds admission, not native cleanup. Timing out the owned
    // open future would drop its cancel-and-join completion while SSH still reaps.
    // Keep the join owner in resources until close drains that same attempt.
    let opened = timeout(
        DIAL_DEADLINE,
        resources.opening.as_mut().ok_or_else(cancelled)?,
    )
    .await
    .map_err(|_| crate::protocol_adapter::unavailable())?;
    resources.opening.take();
    let route = opened.map_err(|_| cancelled())??;
    resources.sender = Some(Arc::clone(&route.sender));
    state.send_replace(ConnectionState::Connecting {
        phase: ConnectingPhase::Authenticating,
    });
    let mut session = timeout(
        HANDSHAKE_DEADLINE,
        ProtocolSession::authenticate(
            target,
            route,
            services.credentials.as_ref(),
            &services.additional_capabilities,
            Arc::clone(&resources.egress),
        ),
    )
    .await
    .map_err(|_| crate::protocol_adapter::unavailable())??;
    resources.writer_done = Some(session.writer.done.clone());
    let mut info = timeout(
        RECONCILIATION_DEADLINE,
        synchronize(shared, binding, state, replica, &mut session, attempt, None),
    )
    .await
    .map_err(|_| crate::protocol_adapter::unavailable())??;
    queries.authenticated(info, &session.codec)?;
    state.send_replace(ConnectionState::Connected { info });
    let mut publication_deadline = None;
    loop {
        let message = tokio::select! {
            message = async {
                if let Some(deadline) = publication_deadline {
                    timeout_at(deadline, session.receive_payload()).await.unwrap_or_else(|_| Err(crate::protocol_adapter::unavailable()))
                } else { session.receive_payload().await }
            } => message,
            request = outgoing.recv() => {
                let request = request.ok_or_else(cancelled)?;
                let admission = if matches!(*state.borrow(), ConnectionState::Connected { .. })
                    && same_connection(info, request.captured)
                {
                    if request.query { queries.admit(info, &request.message, binding) } else { services.delegate.admit(info, &request.message).map(|_| ()) }
                } else {
                    Err(stale())
                };
                if let Err(error) = admission {
                    let _ = request.completion.send(Err(error));
                    continue;
                }
                session.writer.dispatch(
                    request,
                    Arc::clone(&services.delegate),
                    state.subscribe(),
                    Arc::clone(&queries),
                    binding.clone(),
                )?;
                continue;
            },
        };
        let message = message?;
        if confirmed_stopped(&message, info)? {
            services
                .delegate
                .invalidate(binding, ConnectionInvalidation::Stopped);
            queries.invalidate();
            state.send_replace(ConnectionState::Stopped);
            return Ok(());
        }
        if is_organization(&message) {
            // Eligibility goes first, including invalid delta paths. Subscriber
            // notification never becomes a window for old native permits.
            services
                .delegate
                .invalidate(binding, ConnectionInvalidation::Publication);
            let update = match services.organization.decode(message)? {
                OrganizationPublication::Delta(delta) => replica.apply_delta(&delta),
                OrganizationPublication::DeltaPart { stamp, delta } => {
                    replica.stage_delta(stamp, delta)
                }
                OrganizationPublication::Snapshot(_) => Err(invalid_peer()),
            };
            if matches!(update, Ok(ReplicaUpdate::Installed)) {
                let snapshot = replica
                    .subscribe()
                    .borrow()
                    .retained()
                    .cloned()
                    .ok_or_else(invalid_peer)?;
                info.organization_revision = snapshot.cursor.revision;
                services.delegate.publication(info, &snapshot)?;
                publication_deadline = None;
                state.send_replace(ConnectionState::Connected { info });
            } else if matches!(update, Ok(ReplicaUpdate::Staged)) {
                // A partial publication never exposes partial membership.
                publication_deadline
                    .get_or_insert_with(|| Instant::now() + RECONCILIATION_DEADLINE);
                state.send_replace(ConnectionState::Connecting {
                    phase: ConnectingPhase::Reconciling,
                });
                continue;
            } else if matches!(update, Ok(ReplicaUpdate::Ignored)) {
                continue;
            } else {
                queries.invalidate();
                reject_outgoing(outgoing);
                publication_deadline = None;
                info = timeout(
                    RECONCILIATION_DEADLINE,
                    synchronize(
                        shared,
                        binding,
                        state,
                        replica,
                        &mut session,
                        attempt,
                        Some(info),
                    ),
                )
                .await
                .map_err(|_| crate::protocol_adapter::unavailable())??;
                queries.authenticated(info, &session.codec)?;
                state.send_replace(ConnectionState::Connected { info });
            }
        } else if is_resync(&message) {
            services
                .delegate
                .invalidate(binding, ConnectionInvalidation::Reconciliation);
            queries.invalidate();
            reject_outgoing(outgoing);
            publication_deadline = None;
            info = timeout(
                RECONCILIATION_DEADLINE,
                synchronize(
                    shared,
                    binding,
                    state,
                    replica,
                    &mut session,
                    attempt,
                    Some(info),
                ),
            )
            .await
            .map_err(|_| crate::protocol_adapter::unavailable())??;
            queries.authenticated(info, &session.codec)?;
            state.send_replace(ConnectionState::Connected { info });
        } else if queries.receive(info, &message)? {
            continue;
        } else {
            session.bind_delegate(Arc::clone(&services.delegate), info);
            timeout(
                RECONCILIATION_DEADLINE,
                services.delegate.receive(info, message, &mut session),
            )
            .await
            .map_err(|_| crate::protocol_adapter::unavailable())??;
        }
    }
}

async fn synchronize(
    shared: &Shared,
    binding: &PairedServerBinding,
    state: &watch::Sender<ConnectionState>,
    replica: &mut Replica,
    session: &mut ProtocolSession,
    attempt: u64,
    previous: Option<ConnectedInfo>,
) -> Result<ConnectedInfo, ClientFailure> {
    let services = &shared.services;
    let target = binding.target();
    services
        .delegate
        .invalidate(binding, ConnectionInvalidation::Reconciliation);
    state.send_replace(ConnectionState::Connecting {
        phase: ConnectingPhase::Reconciling,
    });
    let epoch = ServerEpoch::new(session.epoch);
    let mut failures = 0u8;
    let mut skipped = 0usize;
    let (snapshot, connection) = 'resync: loop {
        let token = replica.begin_resync(epoch)?;
        session
            .send_payload(&organization_query(session.major, token.get())?)
            .await?;
        loop {
            let payload = session.receive_payload().await?;
            if is_resync(&payload) {
                break;
            }
            if is_organization(&payload) {
                let result = match services.organization.decode(payload)? {
                    OrganizationPublication::Snapshot(part)
                        if part.stamp.request == token.get() =>
                    {
                        replica.stage_snapshot(token, *part)
                    }
                    OrganizationPublication::Snapshot(_)
                    | OrganizationPublication::Delta(_)
                    | OrganizationPublication::DeltaPart { .. } => Ok(ReplicaUpdate::Ignored),
                };
                match result {
                    Ok(ReplicaUpdate::Installed) => {
                        let mut snapshot = replica
                            .subscribe()
                            .borrow()
                            .retained()
                            .cloned()
                            .ok_or_else(invalid_peer)?;
                        session
                            .send_payload(&reconciliation(
                                session.major,
                                token.get(),
                                target.client(),
                                snapshot.cursor,
                            )?)
                            .await?;
                        let mut partial = false;
                        loop {
                            let payload = session.receive_payload().await?;
                            if is_organization(&payload) {
                                let update = match services.organization.decode(payload)? {
                                    OrganizationPublication::Delta(delta) => {
                                        replica.apply_delta(&delta)
                                    }
                                    OrganizationPublication::DeltaPart { stamp, delta } => {
                                        replica.stage_delta(stamp, delta)
                                    }
                                    OrganizationPublication::Snapshot(_) => Err(invalid_peer()),
                                };
                                match update {
                                    Ok(ReplicaUpdate::Installed) => {
                                        partial = false;
                                        snapshot = replica
                                            .subscribe()
                                            .borrow()
                                            .retained()
                                            .cloned()
                                            .ok_or_else(invalid_peer)?;
                                        session
                                            .send_payload(&reconciliation(
                                                session.major,
                                                token.get(),
                                                target.client(),
                                                snapshot.cursor,
                                            )?)
                                            .await?;
                                    }
                                    Ok(ReplicaUpdate::Staged) => partial = true,
                                    Ok(ReplicaUpdate::Ignored) => {}
                                    _ => break,
                                }
                            } else if is_resync(&payload) {
                                break;
                            } else if let Some(connection) = reconciliation_binding(
                                &payload,
                                token.get(),
                                target.client(),
                                snapshot.cursor,
                            )? {
                                if !partial {
                                    break 'resync (snapshot, connection);
                                }
                            } else {
                                skipped += 1;
                                if skipped > spaceterm_protocol::limits::MAX_EGRESS_ENVELOPES {
                                    return Err(invalid_peer());
                                }
                            }
                        }
                        break;
                    }
                    Ok(_) => continue,
                    Err(_) => break,
                }
            } else {
                // An acknowledgement already in flight from the previous request
                // is comparison data only, never readiness for this fresh base.
                skipped += 1;
                if skipped > spaceterm_protocol::limits::MAX_EGRESS_ENVELOPES {
                    return Err(invalid_peer());
                }
            }
        }
        failures += 1;
        if failures >= MAX_RESYNC_FAILURES {
            return Err(invalid_peer());
        }
    };
    if previous.is_some_and(|info| info.binding != connection) {
        return Err(invalid_peer());
    }
    let info = ConnectedInfo {
        server: target.server(),
        pairing: target.pairing(),
        client: target.client(),
        epoch,
        binding: connection,
        major: session.major,
        limits: session.limits,
        capabilities: session.capabilities,
        organization_revision: snapshot.cursor.revision,
        attempt,
    };
    session.bind_delegate(Arc::clone(&services.delegate), info);
    services
        .delegate
        .reconcile(info, &snapshot, session)
        .await?;
    // Tombstones or uncertain journal health can close admission during reconciliation.
    if !services.journal.ordinary_use_allowed(binding).await? {
        return Err(ClientFailure::new(ClientFailureClass::AuthorizationDenied));
    }
    services.delegate.publication(info, &snapshot)?;
    Ok(info)
}
