//! Listener generations follow committed remote policy. IPC and SSH-forwarded
//! IPC remain independent of WSS enablement. Native TLS keys are injected.

use crate::{
    adapters::transport::transport_failure,
    api::{ListenerLifecyclePort, ServerHandle, ServicePhase},
    connection::{
        AuthenticatedPeer, AuthenticationConfig, Authenticator, HANDSHAKE_DEADLINE,
        MAX_UNAUTHENTICATED_CONNECTIONS,
    },
    contracts::*,
    failure::failure,
};
use spaceterm_core::{
    management::{ListenerState, ListenerStatus},
    organization::{OrganizationSnapshot, Versioned},
    remote_access::RemoteAccessPolicy,
    storage::CommitReceipt,
};
use spaceterm_host::{
    ipc::{IpcEndpoint, IpcListener},
    transport::{AdmissionLimits, MessageLimit},
    wss::{RemoteAccessEnablement, WssListener},
};
use std::{
    num::NonZeroU64,
    sync::{Arc, Mutex},
};
use tokio::sync::{Semaphore, watch};
use tokio::task::{JoinHandle, JoinSet};

/// Native composition supplies credential and owning-account facts. The factory
/// must bind the already recovered identity/epoch, never discover another server.
pub trait AuthenticationFactoryPort: Send + Sync {
    fn config(
        &self,
        handle: ServerHandle,
        snapshot: &OrganizationSnapshot,
    ) -> Result<AuthenticationConfig, ServerFailure>;
    /// Enrollment is a separate verified purpose. Native/extension composition
    /// supplies its retained reservation lookup, never an ordinary principal.
    fn enrollment_lookup(
        &self,
        _handle: ServerHandle,
        _snapshot: &OrganizationSnapshot,
    ) -> Result<
        Option<Arc<dyn spaceterm_protocol::pairing_adapter::EnrollmentLookup + Send + Sync>>,
        ServerFailure,
    > {
        Ok(None)
    }
}
/// The connection/publication owner supplies ordinary and restricted dispatch.
/// RestrictedConnection is never converted to an ordinary principal by this port.
pub trait ConnectionDispatchPort: Send + Sync {
    fn run<'a>(
        &'a self,
        peer: &'a mut AuthenticatedPeer,
        handle: ServerHandle,
    ) -> ServerFuture<'a, Result<(), ServerFailure>>;
}

pub trait ConnectionDispatchFactoryPort: Send + Sync {
    fn create(
        &self,
        authenticator: Authenticator,
    ) -> Result<Arc<dyn ConnectionDispatchPort>, ServerFailure>;
}

pub struct ListenerConfig {
    pub ipc: IpcEndpoint,
    pub tls: Arc<rustls::ServerConfig>,
    pub authentication: Arc<dyn AuthenticationFactoryPort>,
    pub dispatch: Arc<dyn ConnectionDispatchFactoryPort>,
}
struct ListenerTask {
    stop: watch::Sender<bool>,
    join: JoinHandle<Result<(), ServerFailure>>,
}
impl ListenerTask {
    async fn close(self) -> Result<(), ServerFailure> {
        self.stop.send_replace(true);
        self.join
            .await
            .map_err(|_| failure(ServerFailureClass::CleanupIncomplete))?
    }
}
enum RemoteInstallation {
    Installed,
    Unavailable(ServerFailure),
}

struct Installed {
    closed: bool,
    opened: bool,
    handle: Option<ServerHandle>,
    authenticator: Option<Authenticator>,
    driver: Option<Arc<dyn ConnectionDispatchPort>>,
    ipc: Option<ListenerTask>,
    remote: Option<ListenerTask>,
    policy: Option<Versioned<RemoteAccessPolicy>>,
}

pub struct ServerListeners {
    config: ListenerConfig,
    installed: Mutex<Installed>,
    policy_effect: Semaphore,
    statuses: watch::Sender<Vec<ListenerStatus>>,
}
impl ServerListeners {
    pub fn new(config: ListenerConfig) -> Self {
        Self {
            config,
            installed: Mutex::new(Installed {
                closed: false,
                opened: false,
                handle: None,
                authenticator: None,
                driver: None,
                ipc: None,
                remote: None,
                policy: None,
            }),
            policy_effect: Semaphore::new(1),
            statuses: watch::channel(Vec::new()).0,
        }
    }
    pub fn subscribe_status(&self) -> watch::Receiver<Vec<ListenerStatus>> {
        self.statuses.subscribe()
    }
    pub fn authenticator(&self) -> Result<Authenticator, ServerFailure> {
        self.installed
            .lock()
            .map_err(|_| failure(ServerFailureClass::RecoveryRequired))?
            .authenticator
            .clone()
            .ok_or_else(|| failure(ServerFailureClass::Authentication))
    }
    pub fn request_close_clients(
        &self,
        clients: &[spaceterm_core::identity::ClientId],
    ) -> Result<(), ServerFailure> {
        let authenticator = self
            .installed
            .lock()
            .map_err(|_| failure(ServerFailureClass::RecoveryRequired))?
            .authenticator
            .clone();
        if let Some(authenticator) = authenticator {
            authenticator.request_close_clients(clients)?;
        }
        Ok(())
    }
    async fn open_listeners(
        &self,
        handle: ServerHandle,
        snapshot: Arc<OrganizationSnapshot>,
    ) -> Result<(), ServerFailure> {
        if !matches!(handle.phase(), ServicePhase::Ready | ServicePhase::Serving)
            || handle.snapshot().as_ref() != snapshot.as_ref()
        {
            return Err(failure(ServerFailureClass::RecoveryRequired));
        }
        let policy = snapshot
            .state()
            .retained
            .as_ref()
            .ok_or_else(|| failure(ServerFailureClass::RecoveryRequired))?
            .remote_access()
            .clone();
        let config = self
            .config
            .authentication
            .config(handle.clone(), &snapshot)?;
        if config.server != snapshot.state().server || config.epoch != snapshot.epoch() {
            return Err(failure(ServerFailureClass::Authentication));
        }
        let enrollment = self
            .config
            .authentication
            .enrollment_lookup(handle.clone(), &snapshot)?;
        let authenticator = match enrollment {
            Some(lookup) => Authenticator::new_with_enrollment(config, lookup)?,
            None => Authenticator::new(config)?,
        };
        let driver = self.config.dispatch.create(authenticator.clone())?;
        let _effect = self
            .policy_effect
            .acquire()
            .await
            .map_err(|_| failure(ServerFailureClass::CleanupIncomplete))?;
        {
            let mut installed = self
                .installed
                .lock()
                .map_err(|_| failure(ServerFailureClass::RecoveryRequired))?;
            if installed.closed || installed.opened {
                return Err(failure(ServerFailureClass::StaleAuthority));
            }
            installed.opened = true;
        }
        let listener = IpcListener::bind(
            self.config.ipc.clone(),
            MessageLimit::envelope(),
            admission()?,
        )
        .map_err(transport_failure)?;
        let ipc = start_ipc(
            listener,
            authenticator.clone(),
            driver.clone(),
            handle.clone(),
        );
        let mut ipc = Some(ipc);
        let closed = {
            let mut installed = self
                .installed
                .lock()
                .map_err(|_| failure(ServerFailureClass::RecoveryRequired))?;
            if installed.closed {
                true
            } else {
                installed.ipc = ipc.take();
                installed.handle = Some(handle.clone());
                installed.authenticator = Some(authenticator.clone());
                installed.driver = Some(driver.clone());
                false
            }
        };
        if closed {
            if let Some(ipc) = ipc {
                ipc.close().await?;
            }
            authenticator.close().await?;
            return Err(failure(ServerFailureClass::StaleAuthority));
        }
        // A recovered, valid enabled policy is still authoritative when its
        // network endpoints are occupied. Keep IPC available to inspect and
        // disable it; only malformed policy or IPC failure blocks startup.
        self.install_remote(policy, handle, authenticator, driver)
            .await
            .map(|_| ())
    }
    async fn apply_policy(
        &self,
        policy: Versioned<RemoteAccessPolicy>,
        receipt: CommitReceipt,
    ) -> Result<(), ServerFailure> {
        let _effect = self
            .policy_effect
            .acquire()
            .await
            .map_err(|_| failure(ServerFailureClass::CleanupIncomplete))?;
        let (handle, authenticator, driver, old) = {
            let mut installed = self
                .installed
                .lock()
                .map_err(|_| failure(ServerFailureClass::RecoveryRequired))?;
            if installed.closed
                || installed
                    .policy
                    .as_ref()
                    .is_some_and(|old| old.version >= policy.version)
            {
                return Err(failure(ServerFailureClass::StaleAuthority));
            }
            let handle = installed
                .handle
                .clone()
                .ok_or_else(|| failure(ServerFailureClass::RecoveryRequired))?;
            let snapshot = handle.snapshot();
            if receipt.store_revision.get() == 0
                || receipt.organization_revision != snapshot.state().revision
                || snapshot
                    .state()
                    .retained
                    .as_ref()
                    .map(|v| v.remote_access())
                    != Some(&policy)
            {
                return Err(failure(ServerFailureClass::StaleAuthority));
            }
            let authenticator = installed
                .authenticator
                .clone()
                .ok_or_else(|| failure(ServerFailureClass::RecoveryRequired))?;
            (
                handle,
                authenticator,
                installed
                    .driver
                    .clone()
                    .ok_or_else(|| failure(ServerFailureClass::RecoveryRequired))?,
                installed.remote.take(),
            )
        };
        if let Some(old) = old {
            old.close().await?;
        }
        match self
            .install_remote(policy, handle, authenticator, driver)
            .await?
        {
            RemoteInstallation::Installed => Ok(()),
            RemoteInstallation::Unavailable(error) => Err(error),
        }
    }
    async fn install_remote(
        &self,
        policy: Versioned<RemoteAccessPolicy>,
        handle: ServerHandle,
        authenticator: Authenticator,
        driver: Arc<dyn ConnectionDispatchPort>,
    ) -> Result<RemoteInstallation, ServerFailure> {
        if policy.value.server() != handle.snapshot().state().server {
            return Err(failure(ServerFailureClass::ScopeMismatch));
        }
        if !policy.value.is_enabled() {
            let mut installed = self
                .installed
                .lock()
                .map_err(|_| failure(ServerFailureClass::RecoveryRequired))?;
            if installed.closed {
                return Err(failure(ServerFailureClass::StaleAuthority));
            }
            installed.policy = Some(policy);
            self.statuses.send_replace(Vec::new());
            return Ok(RemoteInstallation::Installed);
        }
        let generation = NonZeroU64::new(policy.version.get())
            .ok_or_else(|| failure(ServerFailureClass::RecoveryRequired))?;
        let facts = |state| {
            policy
                .value
                .endpoints()
                .iter()
                .copied()
                .map(|endpoint| ListenerStatus {
                    generation,
                    endpoint,
                    state,
                })
                .collect()
        };
        self.statuses.send_replace(facts(ListenerState::Binding));
        let enablement = RemoteAccessEnablement::from_durable_setting(
            policy.value.server().bytes(),
            policy.value.endpoints().to_vec(),
        )
        .map_err(transport_failure)?;
        let listener = WssListener::bind(
            Some(&enablement),
            self.config.tls.clone(),
            MessageLimit::envelope(),
            admission()?,
        )
        .await;
        let listener = match listener {
            Ok(listener) => Arc::new(listener),
            Err(error) => {
                self.statuses
                    .send_replace(facts(ListenerState::Unavailable));
                let mut installed = self
                    .installed
                    .lock()
                    .map_err(|_| failure(ServerFailureClass::RecoveryRequired))?;
                if !installed.closed {
                    installed.policy = Some(policy);
                }
                return match error {
                    spaceterm_host::transport::TransportError::Unavailable => {
                        Ok(RemoteInstallation::Unavailable(transport_failure(error)))
                    }
                    _ => Err(transport_failure(error)),
                };
            }
        };
        let endpoints = listener
            .local_addresses()
            .await
            .map_err(transport_failure)?;
        let remote = start_wss(listener, authenticator, driver, handle);
        let mut remote = Some(remote);
        let closed = {
            let mut installed = self
                .installed
                .lock()
                .map_err(|_| failure(ServerFailureClass::RecoveryRequired))?;
            if installed.closed {
                true
            } else {
                installed.remote = remote.take();
                installed.policy = Some(policy);
                false
            }
        };
        if closed {
            if let Some(remote) = remote {
                remote.close().await?;
            }
            self.statuses.send_replace(Vec::new());
            return Err(failure(ServerFailureClass::StaleAuthority));
        }
        self.statuses.send_replace(
            endpoints
                .into_iter()
                .map(|endpoint| ListenerStatus {
                    generation,
                    endpoint,
                    state: ListenerState::Listening,
                })
                .collect(),
        );
        Ok(RemoteInstallation::Installed)
    }
    async fn close_listeners(&self) -> Result<(), ServerFailure> {
        let (ipc, remote, authenticator) = {
            let mut installed = self
                .installed
                .lock()
                .map_err(|_| failure(ServerFailureClass::RecoveryRequired))?;
            installed.closed = true;
            (
                installed.ipc.take(),
                installed.remote.take(),
                installed.authenticator.clone(),
            )
        };
        let mut failure_result = None;
        if let Some(remote) = remote
            && let Err(error) = remote.close().await
        {
            failure_result = Some(error);
        }
        if let Some(ipc) = ipc
            && let Err(error) = ipc.close().await
        {
            failure_result = Some(error);
        }
        if let Some(authenticator) = authenticator
            && let Err(error) = authenticator.close().await
        {
            failure_result = Some(error);
        }
        // Wait for an in-progress bind to notice closed and tear down its generation.
        let _effect = self
            .policy_effect
            .acquire()
            .await
            .map_err(|_| failure(ServerFailureClass::CleanupIncomplete))?;
        self.statuses.send_replace(Vec::new());
        failure_result.map_or(Ok(()), Err)
    }
}
impl ListenerLifecyclePort for ServerListeners {
    fn open(
        &self,
        handle: ServerHandle,
        snapshot: Arc<OrganizationSnapshot>,
    ) -> ServerFuture<'_, Result<(), ServerFailure>> {
        Box::pin(self.open_listeners(handle, snapshot))
    }
    fn apply_remote_policy(
        &self,
        policy: Versioned<RemoteAccessPolicy>,
        receipt: CommitReceipt,
    ) -> ServerFuture<'_, Result<(), ServerFailure>> {
        Box::pin(self.apply_policy(policy, receipt))
    }
    fn close(&self) -> ServerFuture<'_, Result<(), ServerFailure>> {
        Box::pin(self.close_listeners())
    }
}

fn admission() -> Result<AdmissionLimits, ServerFailure> {
    AdmissionLimits::new(MAX_UNAUTHENTICATED_CONNECTIONS, HANDSHAKE_DEADLINE)
        .map_err(transport_failure)
}

async fn dispatch(
    mut peer: AuthenticatedPeer,
    driver: Arc<dyn ConnectionDispatchPort>,
    handle: ServerHandle,
    mut stop: watch::Receiver<bool>,
) -> Result<(), ServerFailure> {
    let mut closing = peer.closing.clone();
    let result = tokio::select! { biased; _ = stop.wait_for(|value| *value) => Ok(()), _ = closing.wait_for(|value| *value) => Ok(()), result = driver.run(&mut peer,handle) => result };
    peer.close().await?;
    result
}

fn start_ipc(
    listener: IpcListener,
    authenticator: Authenticator,
    driver: Arc<dyn ConnectionDispatchPort>,
    handle: ServerHandle,
) -> ListenerTask {
    let (stop, mut stopping) = watch::channel(false);
    let children_stop = stopping.clone();
    let join = tokio::spawn(async move {
        let mut children = JoinSet::new();
        loop {
            let permit = tokio::select! { biased; _ = stopping.wait_for(|value| *value) => break, result = authenticator.admit() => match result { Ok(permit) => permit, Err(_) => break } };
            let pending = tokio::select! { biased; _ = stopping.wait_for(|value| *value) => { drop(permit); break; }, result = listener.accept() => result };
            match pending {
                Ok(pending) => {
                    let authenticator = authenticator.clone();
                    let driver = driver.clone();
                    let handle = handle.clone();
                    let stop = children_stop.clone();
                    children.spawn(async move {
                        let auth = authenticator
                            .authenticate_until(pending, permit, stop.clone())
                            .await
                            .ok();
                        if let Some(peer) = auth {
                            let _ = dispatch(peer, driver, handle, stop).await;
                        }
                    });
                }
                Err(error) => {
                    drop(permit);
                    if matches!(
                        error,
                        spaceterm_host::transport::TransportError::UnsafeEndpoint
                            | spaceterm_host::transport::TransportError::Closed
                            | spaceterm_host::transport::TransportError::RemoteAccessDisabled
                            | spaceterm_host::transport::TransportError::Unavailable
                    ) {
                        break;
                    }
                }
            }
            while children.try_join_next().is_some() {}
        }
        while children.join_next().await.is_some() {}
        listener
            .close()
            .map_err(|_| failure(ServerFailureClass::CleanupIncomplete))
    });
    ListenerTask { stop, join }
}
fn start_wss(
    listener: Arc<WssListener>,
    authenticator: Authenticator,
    driver: Arc<dyn ConnectionDispatchPort>,
    handle: ServerHandle,
) -> ListenerTask {
    let (stop, mut stopping) = watch::channel(false);
    let children_stop = stopping.clone();
    let join = tokio::spawn(async move {
        let mut children = JoinSet::new();
        loop {
            let permit = tokio::select! { biased; _ = stopping.wait_for(|value| *value) => break, result = authenticator.admit() => match result { Ok(permit) => permit, Err(_) => break } };
            let pending = tokio::select! { biased; _ = stopping.wait_for(|value| *value) => { drop(permit); break; }, result = listener.accept() => result };
            match pending {
                Ok(pending) => {
                    let authenticator = authenticator.clone();
                    let driver = driver.clone();
                    let handle = handle.clone();
                    let stop = children_stop.clone();
                    children.spawn(async move {
                        let auth = authenticator
                            .authenticate_until(pending, permit, stop.clone())
                            .await
                            .ok();
                        if let Some(peer) = auth {
                            let _ = dispatch(peer, driver, handle, stop).await;
                        }
                    });
                }
                Err(error) => {
                    drop(permit);
                    if matches!(
                        error,
                        spaceterm_host::transport::TransportError::UnsafeEndpoint
                            | spaceterm_host::transport::TransportError::RemoteAccessDisabled
                    ) {
                        break;
                    }
                    // Closed and Unavailable also classify a single peer's
                    // failed WebSocket upgrade. Neither revokes the listener.
                    // Back off transient accept resource failures as well.
                    tokio::select! {
                        biased;
                        _ = stopping.wait_for(|value| *value) => break,
                        _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => {}
                    }
                }
            }
            while children.try_join_next().is_some() {}
        }
        listener.disable().await;
        while children.join_next().await.is_some() {}
        Ok(())
    });
    ListenerTask { stop, join }
}
