//! Production composition reuses actor authentication, pairing and publication.

use spaceterm_core::{
    management::{
        MAX_STATUS_PROGRESS, ManagementProgress, ManagementProgressState,
        ManagementRequestReference, ServerRuntimeStatus, ServerStatus, ServerStatusParts,
        ServerVersion,
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
    ListenerConfig, NativeStatusObservation, NativeStatusObserver, ServerListeners,
    outcomes::ConnectionOutcomes,
};
use crate::{
    adapters::protocol::{OrdinaryConnectionRevocationPort, PairingConnectionObserver},
    api::{ListenerFactories, ListenerLifecyclePort, ServerHandle},
    connection::{
        AuthenticationConfig, Authenticator, HANDSHAKE_DEADLINE, MAX_AUTHENTICATED_CONNECTIONS,
        MAX_UNAUTHENTICATED_CONNECTIONS,
    },
    contracts::{ServerClock, ServerFailure, ServerFailureClass, ServerFuture},
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
    account_runtime: Mutex<Option<Arc<crate::account_runtime::AccountRuntimeProcesses>>>,
    native: Option<Arc<dyn NativeStatusObserver>>,
    protocol_majors: Option<Vec<u32>>,
}
impl ProductionListenerPorts {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    /// Binds the one recovered account runtime before the paired listener opens.
    pub fn install_account_runtime(
        &self,
        runtime: Arc<crate::account_runtime::AccountRuntimeProcesses>,
    ) -> Result<(), ServerFailure> {
        let mut slot = self
            .account_runtime
            .lock()
            .map_err(|_| failure(ServerFailureClass::RecoveryRequired))?;
        if slot.is_some() {
            return Err(failure(ServerFailureClass::StaleAuthority));
        }
        *slot = Some(runtime);
        Ok(())
    }
    pub fn with_native_status(native: Arc<dyn NativeStatusObserver>) -> Arc<Self> {
        Arc::new(Self {
            native: Some(native),
            ..Self::default()
        })
    }
    /// Restricts negotiation to supported profiles for a rolling-version host.
    pub fn with_native_status_and_protocol_majors(
        native: Arc<dyn NativeStatusObserver>,
        protocol_majors: Vec<u32>,
    ) -> Result<Arc<Self>, ServerFailure> {
        let supported = HandshakeConfig::default().majors;
        if protocol_majors.is_empty()
            || protocol_majors
                .iter()
                .any(|major| !supported.contains(major))
            || protocol_majors
                .iter()
                .enumerate()
                .any(|(index, major)| protocol_majors[..index].contains(major))
        {
            return Err(failure(ServerFailureClass::ScopeMismatch));
        }
        Ok(Arc::new(Self {
            native: Some(native),
            protocol_majors: Some(protocol_majors),
            ..Self::default()
        }))
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
        &NativeStatusObservation::default(),
        handle.management_progress(),
    )?;
    let key = config.registration.key();
    let initial = attach_machine_facts(
        initial,
        config
            .ports
            .native
            .as_ref()
            .and_then(|native| native.machine_facts(key)),
    )?;
    let (status_sender, status_receiver) = watch::channel(initial);
    let account_runtime = config
        .ports
        .account_runtime
        .lock()
        .map_err(|_| failure(ServerFailureClass::RecoveryRequired))?
        .clone();
    let authentication = Arc::new(ProductionAuthentication {
        uid: config.registration.key().account_uid,
        account_runtime: account_runtime.is_some(),
        extensions: ordinary.capabilities(),
        protocol_majors: config.ports.protocol_majors.clone(),
        identity: config.identity.clone(),
        lookup,
        server: snapshot.state().server,
        epoch: snapshot.epoch(),
    });
    let dispatch = Arc::new(ProductionDispatch {
        clock: handle.clock.clone(),
        status: status_receiver.clone(),
        ordinary: Arc::new(PairingOrdinaryDispatch::new(
            config.pairing.clone(),
            ordinary.clone(),
        )),
        pairing: config.pairing,
        outcomes: config.ports.outcomes.clone(),
        account_runtime,
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
            ordinary,
            handle,
            running: config.running,
            status: status_sender,
            native: config.ports.native.clone(),
            key,
            native_facts: Arc::new(Mutex::new(NativeStatusObservation::default())),
            monitor: Mutex::new(None),
            stop: watch::channel(false).0,
        }),
    })
}

struct ProductionAuthentication<S> {
    extensions: Vec<u32>,
    protocol_majors: Option<Vec<u32>>,
    account_runtime: bool,
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
        let mut capabilities = vec![
            spaceterm_protocol::negotiation::CAPABILITY_NORMALIZED_CREATION_TOPOLOGY,
            spaceterm_protocol::negotiation::CAPABILITY_CANONICAL_DOMAIN,
            spaceterm_protocol::negotiation::CAPABILITY_ORGANIZATION_COMPARISONS,
            spaceterm_protocol::negotiation::CAPABILITY_TERMINAL_STREAMS,
            spaceterm_protocol::negotiation::CAPABILITY_NATIVE_NOTIFICATIONS,
        ];
        if self.account_runtime {
            capabilities.push(spaceterm_protocol::negotiation::CAPABILITY_ACCOUNT_RUNTIME);
            capabilities.push(spaceterm_protocol::negotiation::CAPABILITY_REMOTE_NATIVE_LIFECYCLE);
        }
        capabilities.extend([
            spaceterm_protocol::negotiation::CAPABILITY_APPLICATION_MOUSE,
            spaceterm_protocol::negotiation::CAPABILITY_RUNTIME_METADATA,
            spaceterm_protocol::negotiation::CAPABILITY_RUNTIME_METADATA_FRAGMENTS,
            spaceterm_protocol::negotiation::CAPABILITY_NATIVE_PASTE_CORRELATION,
            spaceterm_protocol::negotiation::CAPABILITY_ENHANCED_COPY,
            spaceterm_protocol::negotiation::CAPABILITY_SERVER_MACHINE_FACTS,
        ]);
        capabilities.extend_from_slice(&self.extensions);
        capabilities.sort_unstable();
        capabilities.dedup();
        Ok(AuthenticationConfig {
            server: self.server,
            epoch: self.epoch,
            owning_uid: self.uid,
            handshake: HandshakeConfig {
                majors: self
                    .protocol_majors
                    .clone()
                    .unwrap_or_else(|| HandshakeConfig::default().majors),
                capabilities,
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
    clock: Arc<dyn ServerClock>,
    account_runtime: Option<Arc<crate::account_runtime::AccountRuntimeProcesses>>,
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
        let mut dispatcher = ProtocolDispatcher::new(
            authenticator,
            self.status.clone(),
            self.ordinary.clone(),
            Arc::new(RejectRestrictedFallback),
        )
        .with_clock(self.clock.clone())
        .with_pairing(self.pairing.clone())
        .with_outcomes(self.outcomes.clone());
        if let Some(runtime) = &self.account_runtime {
            dispatcher = dispatcher.with_account_runtime(runtime.clone());
        }
        Ok(Arc::new(dispatcher))
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
    native: &NativeStatusObservation,
    mut progress: Vec<ManagementProgress>,
) -> Result<ServerStatus, ServerFailure> {
    let retained = snapshot
        .state()
        .retained
        .as_ref()
        .ok_or_else(|| failure(ServerFailureClass::RecoveryRequired))?;
    for observed in &native.progress {
        if progress.len() == MAX_STATUS_PROGRESS {
            break;
        }
        if !progress
            .iter()
            .any(|current| current.request == observed.request)
        {
            progress.push(*observed);
        }
    }
    progress.truncate(MAX_STATUS_PROGRESS);
    ServerStatus::new(ServerStatusParts {
        runtime,
        server: snapshot.state().server,
        epoch: snapshot.epoch(),
        revision,
        running: Some(
            ServerVersion::new(running.as_str().to_owned())
                .map_err(|_| failure(ServerFailureClass::ScopeMismatch))?,
        ),
        approved: native.approved.clone(),
        staged: native.staged.clone(),
        native_verification: native.verification,
        capabilities: native.capabilities.clone(),
        remote_access: retained.remote_access().clone(),
        listeners,
        progress,
    })
    .map_err(|_| failure(ServerFailureClass::RecoveryRequired))
}
pub(crate) fn retirement_progress(
    request: ManagementRequestReference,
    sessions: &[spaceterm_core::retirement::RetiredSession],
) -> ManagementProgress {
    use spaceterm_core::retirement::RetirementCleanupState;
    let completed_units = sessions
        .iter()
        .filter(|session| matches!(session.cleanup(), RetirementCleanupState::Confirmed(_)))
        .count() as u32;
    let state = if sessions
        .iter()
        .any(|session| matches!(session.cleanup(), RetirementCleanupState::Failed(_)))
    {
        ManagementProgressState::RecoveryRequired
    } else {
        // Confirmed cleanup still awaits the owning operation's durable settlement.
        ManagementProgressState::WaitingForCleanup
    };
    ManagementProgress {
        request,
        state,
        completed_units,
        total_units: sessions.len() as u32,
    }
}

fn attach_machine_facts(
    status: ServerStatus,
    facts: Option<spaceterm_core::management::ServerMachineFacts>,
) -> Result<ServerStatus, ServerFailure> {
    match facts {
        Some(facts) => status
            .with_machine_facts(facts)
            .map_err(|_| failure(ServerFailureClass::ScopeMismatch)),
        None => Ok(status),
    }
}

struct StatusListeners {
    ordinary: Arc<dyn OrdinaryDispatchPort>,
    listeners: Arc<ServerListeners>,
    handle: ServerHandle,
    running: spaceterm_distribution::BuildVersion,
    status: watch::Sender<ServerStatus>,
    native: Option<Arc<dyn NativeStatusObserver>>,
    native_facts: Arc<Mutex<NativeStatusObservation>>,
    key: spaceterm_host::startup::AccountBuildKey,
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
                let mut progress = handle.subscribe_management_progress();
                let mut listeners = self.listeners.subscribe_status();
                let mut stop = self.stop.subscribe();
                let sender = self.status.clone();
                let running = self.running.clone();
                let source = handle.clone();
                let native = self.native.clone();
                let key = self.key;
                let native_facts = self.native_facts.clone();
                *monitor = Some(tokio::spawn(async move {
                    let mut poll = tokio::time::interval(std::time::Duration::from_secs(2));
                    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                    loop {
                        tokio::select! {
                            _ = async { let _ = stop.wait_for(|closed| *closed).await; } => return Ok(()),
                            changed = organization.changed() => { if changed.is_err() { return Ok(()); } },
                            changed = listeners.changed() => { if changed.is_err() { return Ok(()); } },
                            changed = runtime.changed() => { if changed.is_err() { return Ok(()); } },
                            changed = progress.changed() => { if changed.is_err() { return Ok(()); } },
                            _ = poll.tick(), if native.is_some() => {
                                if let Some(provider) = &native {
                                    let observation = tokio::select! {
                                        _ = async { let _ = stop.wait_for(|closed| *closed).await; } => return Ok(()),
                                        result = tokio::time::timeout(std::time::Duration::from_secs(5), provider.observe(source.snapshot().state().server)) => result.ok().and_then(Result::ok).unwrap_or_else(NativeStatusObservation::unavailable),
                                    };
                                    *native_facts.lock().map_err(|_| failure(ServerFailureClass::RecoveryRequired))? = observation;
                                }
                            },
                        }
                        organization.mark_unchanged();
                        listeners.mark_unchanged();
                        runtime.mark_unchanged();
                        progress.mark_unchanged();
                        publish_status(
                            &sender,
                            &source,
                            &running,
                            &listeners,
                            &native_facts,
                            native
                                .as_ref()
                                .and_then(|provider| provider.machine_facts(key)),
                        )?;
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
            let providers = self.ordinary.shutdown().await;
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
            closed.and(providers)
        })
    }
}

impl StatusListeners {
    fn publish_current(&self) -> Result<(), ServerFailure> {
        publish_status(
            &self.status,
            &self.handle,
            &self.running,
            &self.listeners.subscribe_status(),
            &self.native_facts,
            self.native
                .as_ref()
                .and_then(|provider| provider.machine_facts(self.key)),
        )
    }
}

fn publish_status(
    sender: &watch::Sender<ServerStatus>,
    handle: &ServerHandle,
    running: &spaceterm_distribution::BuildVersion,
    listeners: &watch::Receiver<Vec<spaceterm_core::management::ListenerStatus>>,
    native: &Mutex<NativeStatusObservation>,
    machine_facts: Option<spaceterm_core::management::ServerMachineFacts>,
) -> Result<(), ServerFailure> {
    let mut result = Ok(());
    sender.send_if_modified(|current| {
        let replacement = (|| {
            let next = status(
                &handle.snapshot(),
                current.parts().revision,
                running,
                listeners.borrow().clone(),
                handle.runtime_status(),
                &*native
                    .lock()
                    .map_err(|_| failure(ServerFailureClass::RecoveryRequired))?,
                handle.management_progress(),
            )?;
            let next = attach_machine_facts(next, machine_facts)?;
            if next == *current {
                return Ok(None);
            }
            let mut parts = next.parts().clone();
            parts.revision = parts
                .revision
                .checked_add(1)
                .ok_or_else(|| failure(ServerFailureClass::Exhausted))?;
            let revised = ServerStatus::new(parts)
                .map_err(|_| failure(ServerFailureClass::RecoveryRequired))?;
            attach_machine_facts(revised, next.machine_facts().cloned()).map(Some)
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
