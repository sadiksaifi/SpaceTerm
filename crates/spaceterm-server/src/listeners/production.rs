//! Production composition reuses actor authentication, pairing and publication.

use spaceterm_core::{
    management::{
        NativeVerification, ServerRuntimeStatus, ServerStatus, ServerStatusParts, ServerVersion,
    },
    operation_ledger_contracts::LedgerCodec,
    organization::OrganizationSnapshot,
    organization::Versioned,
    remote_access::RemoteAccessPolicy,
    storage::CommitReceipt,
};
use spaceterm_host::{
    account_service::RegisteredServer, server_keys::ServerIdentity, wss::server_tls_config,
};
use spaceterm_protocol::{authentication::HandshakeConfig, pairing_adapter::EnrollmentLookup};
use std::{
    num::NonZeroU64,
    sync::{Arc, Mutex, Weak},
};
use tokio::{sync::watch, task::JoinHandle};

use super::{
    AuthenticationFactoryPort, ConnectionDispatchFactoryPort, ConnectionDispatchPort,
    ListenerConfig, ServerListeners, outcomes::ConnectionOutcomes,
};
use crate::{
    adapters::protocol::{OrdinaryConnectionRevocationPort, PairingConnectionObserver},
    api::{ListenerFactories, ListenerLifecyclePort, ServerHandle},
    connection::{
        AuthenticationConfig, Authenticator, HANDSHAKE_DEADLINE, MAX_AUTHENTICATED_CONNECTIONS,
        MAX_UNAUTHENTICATED_CONNECTIONS,
    },
    contracts::{ServerFailure, ServerFailureClass, ServerFuture},
    extensions::ExtensionObserverPort,
    failure::failure,
    pairing::{
        dispatch::PairingOrdinaryDispatch,
        protocol_lookup::{PairingProtocolLookup, PairingSnapshotPort},
        service::PairingService,
    },
    protocol_dispatch::{
        ActorOrdinaryDispatch, OrdinaryDispatchPort, ProtocolDispatcher, RestrictedDispatchPort,
    },
};

/// Install these shared ports in ServerConfig before recovery. The constructor
/// binds the same ports to the recovered listener generation afterward.
#[derive(Default)]
pub struct ProductionListenerPorts {
    pub outcomes: Arc<ConnectionOutcomes>,
    revocations: Arc<ListenerRevocations>,
    status: Mutex<Option<watch::Receiver<ServerStatus>>>,
}
impl ProductionListenerPorts {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    /// The same bounded feed used by authenticated clients, retained after close.
    /// Status is comparison data and grants no native-management authority.
    pub fn subscribe_status(&self) -> Result<watch::Receiver<ServerStatus>, ServerFailure> {
        self.status
            .lock()
            .map_err(|_| failure(ServerFailureClass::RecoveryRequired))?
            .clone()
            .ok_or_else(|| failure(ServerFailureClass::RecoveryRequired))
    }
    pub fn extensions(
        &self,
        inner: Arc<dyn ExtensionObserverPort>,
    ) -> Arc<dyn ExtensionObserverPort> {
        Arc::new(PairingConnectionObserver::new(
            inner,
            self.revocations.clone(),
        ))
    }
}
#[derive(Default)]
struct ListenerRevocations(Mutex<Weak<ServerListeners>>);
impl OrdinaryConnectionRevocationPort for ListenerRevocations {
    fn request_close(
        &self,
        clients: &[spaceterm_core::identity::ClientId],
    ) -> Result<(), ServerFailure> {
        let listeners = self
            .0
            .lock()
            .map_err(|_| failure(ServerFailureClass::RecoveryRequired))?
            .upgrade();
        if let Some(listeners) = listeners {
            listeners.request_close_clients(clients)?;
        }
        Ok(())
    }
}

/// The validated registration and retained credential come from account startup.
/// `ports` must be the same instance installed in the recovered ServerConfig.
pub struct ProductionListenerConfig<'a> {
    pub registration: &'a RegisteredServer,
    pub identity: Arc<ServerIdentity>,
    pub running: spaceterm_distribution::BuildVersion,
    pub codec: Arc<dyn LedgerCodec>,
    pub pairing: PairingService,
    pub ports: Arc<ProductionListenerPorts>,
}

/// Builds IPC plus durably enabled WSS for the already recovered actor. Neither
/// this constructor nor pairing synthesizes remote-access enablement.
pub fn production_listener_factories<S: PairingSnapshotPort + 'static>(
    handle: ServerHandle,
    config: ProductionListenerConfig<'_>,
    lookup: Arc<PairingProtocolLookup<S>>,
) -> Result<ListenerFactories, ServerFailure> {
    let ordinary = Arc::new(ActorOrdinaryDispatch::new(config.codec.clone()));
    production_listener_factories_with_dispatch(handle, config, lookup, ordinary)
}

/// Adds real Git or management dispatch while retaining the same pairing and
/// authentication composition. The fallback never receives restricted traffic.
pub fn production_listener_factories_with_dispatch<S: PairingSnapshotPort + 'static>(
    handle: ServerHandle,
    config: ProductionListenerConfig<'_>,
    lookup: Arc<PairingProtocolLookup<S>>,
    ordinary: Arc<dyn OrdinaryDispatchPort>,
) -> Result<ListenerFactories, ServerFailure> {
    let snapshot = handle.snapshot();
    let pin = config.registration.server_pin();
    if pin != config.identity.pin()
        || pin != config.pairing.pin()
        || pin.identity != snapshot.state().server.bytes()
    {
        return Err(failure(ServerFailureClass::Authentication));
    }
    let initial = status(
        &snapshot,
        NonZeroU64::MIN,
        &config.running,
        Vec::new(),
        handle.runtime_status(),
    )?;
    let (status_sender, status_receiver) = watch::channel(initial);
    let authentication = Arc::new(ProductionAuthentication {
        uid: config.registration.key().account_uid,
        identity: config.identity.clone(),
        lookup,
        server: snapshot.state().server,
        epoch: snapshot.epoch(),
    });
    let dispatch = Arc::new(ProductionDispatch {
        status: status_receiver.clone(),
        ordinary: Arc::new(PairingOrdinaryDispatch::new(
            config.pairing.clone(),
            ordinary,
        )),
        pairing: config.pairing,
        outcomes: config.ports.outcomes.clone(),
    });
    let listeners = Arc::new(ServerListeners::new(ListenerConfig {
        ipc: config
            .registration
            .endpoint()
            .map_err(|_| failure(ServerFailureClass::NativeUnavailable))?,
        tls: server_tls_config(config.identity)
            .map_err(crate::adapters::transport::transport_failure)?,
        authentication,
        dispatch,
    }));
    let mut revocations = config
        .ports
        .revocations
        .0
        .lock()
        .map_err(|_| failure(ServerFailureClass::RecoveryRequired))?;
    if revocations.upgrade().is_some() {
        return Err(failure(ServerFailureClass::StaleAuthority));
    }
    *revocations = Arc::downgrade(&listeners);
    drop(revocations);
    *config
        .ports
        .status
        .lock()
        .map_err(|_| failure(ServerFailureClass::RecoveryRequired))? = Some(status_receiver);
    Ok(ListenerFactories {
        lifecycle: Arc::new(StatusListeners {
            listeners,
            handle,
            running: config.running,
            status: status_sender,
            monitor: Mutex::new(None),
            stop: watch::channel(false).0,
        }),
    })
}

struct ProductionAuthentication<S> {
    uid: u32,
    identity: Arc<ServerIdentity>,
    lookup: Arc<PairingProtocolLookup<S>>,
    server: spaceterm_core::identity::ServerId,
    epoch: spaceterm_core::authority::ServerEpoch,
}
impl<S: PairingSnapshotPort + 'static> AuthenticationFactoryPort for ProductionAuthentication<S> {
    fn config(
        &self,
        handle: ServerHandle,
        snapshot: &OrganizationSnapshot,
    ) -> Result<AuthenticationConfig, ServerFailure> {
        if snapshot.state().server != self.server
            || snapshot.epoch() != self.epoch
            || handle.snapshot().as_ref() != snapshot
        {
            return Err(failure(ServerFailureClass::StaleAuthority));
        }
        Ok(AuthenticationConfig {
            server: self.server,
            epoch: self.epoch,
            owning_uid: self.uid,
            handshake: HandshakeConfig {
                capabilities: vec![
                    spaceterm_protocol::negotiation::CAPABILITY_NORMALIZED_CREATION_TOPOLOGY,
                    spaceterm_protocol::negotiation::CAPABILITY_CANONICAL_DOMAIN,
                    spaceterm_protocol::negotiation::CAPABILITY_ORGANIZATION_COMPARISONS,
                ],
                ..HandshakeConfig::default()
            },
            credentials: self.identity.clone(),
            pairing: self.lookup.clone(),
            authority: Arc::new(handle),
            unauthenticated_limit: MAX_UNAUTHENTICATED_CONNECTIONS,
            authenticated_limit: MAX_AUTHENTICATED_CONNECTIONS,
            deadline: HANDSHAKE_DEADLINE,
        })
    }
    fn enrollment_lookup(
        &self,
        _handle: ServerHandle,
        _snapshot: &OrganizationSnapshot,
    ) -> Result<Option<Arc<dyn EnrollmentLookup + Send + Sync>>, ServerFailure> {
        Ok(Some(self.lookup.clone()))
    }
}
struct ProductionDispatch {
    status: watch::Receiver<ServerStatus>,
    ordinary: Arc<dyn crate::protocol_dispatch::OrdinaryDispatchPort>,
    pairing: PairingService,
    outcomes: Arc<ConnectionOutcomes>,
}
impl ConnectionDispatchFactoryPort for ProductionDispatch {
    fn create(
        &self,
        authenticator: Authenticator,
    ) -> Result<Arc<dyn ConnectionDispatchPort>, ServerFailure> {
        Ok(Arc::new(
            ProtocolDispatcher::new(
                authenticator,
                self.status.clone(),
                self.ordinary.clone(),
                Arc::new(RejectRestrictedFallback),
            )
            .with_pairing(self.pairing.clone())
            .with_outcomes(self.outcomes.clone()),
        ))
    }
}
struct RejectRestrictedFallback;
impl RestrictedDispatchPort for RejectRestrictedFallback {
    fn dispatch(
        &self,
        _principal: spaceterm_protocol::authentication::Principal,
        _payload: spaceterm_protocol::wire::common::RestrictedMessage,
    ) -> ServerFuture<'_, Result<spaceterm_protocol::wire::common::RestrictedMessage, ServerFailure>>
    {
        Box::pin(async { Err(failure(ServerFailureClass::Authentication)) })
    }
}

fn status(
    snapshot: &OrganizationSnapshot,
    revision: NonZeroU64,
    running: &spaceterm_distribution::BuildVersion,
    listeners: Vec<spaceterm_core::management::ListenerStatus>,
    runtime: ServerRuntimeStatus,
) -> Result<ServerStatus, ServerFailure> {
    let retained = snapshot
        .state()
        .retained
        .as_ref()
        .ok_or_else(|| failure(ServerFailureClass::RecoveryRequired))?;
    ServerStatus::new(ServerStatusParts {
        runtime,
        server: snapshot.state().server,
        epoch: snapshot.epoch(),
        revision,
        running: Some(
            ServerVersion::new(running.as_str().to_owned())
                .map_err(|_| failure(ServerFailureClass::ScopeMismatch))?,
        ),
        // No native live installation observation is implied by IPC or by the
        // startup registration. An update may have changed it since recovery.
        approved: None,
        staged: None,
        native_verification: NativeVerification::Unverified,
        capabilities: Vec::new(),
        remote_access: retained.remote_access().clone(),
        listeners,
        progress: Vec::new(),
    })
    .map_err(|_| failure(ServerFailureClass::RecoveryRequired))
}
struct StatusListeners {
    listeners: Arc<ServerListeners>,
    handle: ServerHandle,
    running: spaceterm_distribution::BuildVersion,
    status: watch::Sender<ServerStatus>,
    monitor: Mutex<Option<JoinHandle<Result<(), ServerFailure>>>>,
    stop: watch::Sender<bool>,
}
impl ListenerLifecyclePort for StatusListeners {
    fn open(
        &self,
        handle: ServerHandle,
        snapshot: Arc<OrganizationSnapshot>,
    ) -> ServerFuture<'_, Result<(), ServerFailure>> {
        Box::pin(async move {
            {
                let mut monitor = self
                    .monitor
                    .lock()
                    .map_err(|_| failure(ServerFailureClass::RecoveryRequired))?;
                if monitor.is_some() || *self.stop.borrow() {
                    return Err(failure(ServerFailureClass::StaleAuthority));
                }
                let mut organization = handle.subscribe_snapshot();
                let mut runtime = handle.subscribe_runtime_status();
                let mut listeners = self.listeners.subscribe_status();
                let mut stop = self.stop.subscribe();
                let sender = self.status.clone();
                let running = self.running.clone();
                *monitor = Some(tokio::spawn(async move {
                    loop {
                        tokio::select! { biased;
                            _ = stop.wait_for(|closed| *closed) => return Ok(()),
                            changed = organization.changed() => { if changed.is_err() { return Ok(()); } },
                            changed = listeners.changed() => { if changed.is_err() { return Ok(()); } },
                            changed = runtime.changed() => { if changed.is_err() { return Ok(()); } },
                        }
                        let snapshot = organization.borrow_and_update().clone();
                        let facts = listeners.borrow_and_update().clone();
                        let runtime = *runtime.borrow_and_update();
                        publish_status(&sender, &snapshot, &running, facts, runtime)?;
                    }
                }));
            }
            self.listeners.open(handle, snapshot).await
        })
    }
    fn apply_remote_policy(
        &self,
        policy: Versioned<RemoteAccessPolicy>,
        receipt: CommitReceipt,
    ) -> ServerFuture<'_, Result<(), ServerFailure>> {
        Box::pin(async move {
            let result = self.listeners.apply_remote_policy(policy, receipt).await;
            // Committed policy and final bind facts precede the operation reply.
            self.publish_current()?;
            result
        })
    }
    fn close(&self) -> ServerFuture<'_, Result<(), ServerFailure>> {
        Box::pin(async move {
            let closed = self.listeners.close().await;
            // Keep the last status truthful after the endpoint is gone.
            self.publish_current()?;
            self.stop.send_replace(true);
            let monitor = self
                .monitor
                .lock()
                .map_err(|_| failure(ServerFailureClass::RecoveryRequired))?
                .take();
            if let Some(monitor) = monitor {
                monitor
                    .await
                    .map_err(|_| failure(ServerFailureClass::CleanupIncomplete))??;
            }
            closed
        })
    }
}

impl StatusListeners {
    fn publish_current(&self) -> Result<(), ServerFailure> {
        publish_status(
            &self.status,
            &self.handle.snapshot(),
            &self.running,
            self.listeners.subscribe_status().borrow().clone(),
            self.handle.runtime_status(),
        )
    }
}

fn publish_status(
    sender: &watch::Sender<ServerStatus>,
    snapshot: &OrganizationSnapshot,
    running: &spaceterm_distribution::BuildVersion,
    listeners: Vec<spaceterm_core::management::ListenerStatus>,
    runtime: ServerRuntimeStatus,
) -> Result<(), ServerFailure> {
    let mut result = Ok(());
    sender.send_if_modified(|current| {
        let replacement = (|| {
            let next = status(
                snapshot,
                current.parts().revision,
                running,
                listeners,
                runtime,
            )?;
            if next == *current {
                return Ok(None);
            }
            let mut parts = next.parts().clone();
            parts.revision = parts
                .revision
                .checked_add(1)
                .ok_or_else(|| failure(ServerFailureClass::Exhausted))?;
            ServerStatus::new(parts)
                .map(Some)
                .map_err(|_| failure(ServerFailureClass::RecoveryRequired))
        })();
        match replacement {
            Ok(Some(next)) => {
                *current = next;
                true
            }
            Ok(None) => false,
            Err(error) => {
                result = Err(error);
                false
            }
        }
    });
    result
}
