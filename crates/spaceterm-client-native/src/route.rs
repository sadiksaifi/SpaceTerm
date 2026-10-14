//! Native reachability adapters shared by desktop, companion and CLI. Every
//! returned route still requires the portable client's pinned protocol handshake.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::rand::{SecureRandom, SystemRandom};
use spaceterm_client::{
    api::{ConnectionTarget, RouteDescription, RouteParts},
    failure::{ClientFailure, ClientFailureClass},
    limits::DIAL_DEADLINE,
    pairing::credentials::{CredentialUse, CredentialUsePurpose, NativeCredentialAccess},
    ports::{
        ClientFuture, ConnectedRoute, ConnectionPurpose, ForegroundCancellation, MessageReceiver,
        MessageSender, PeerFacts, RouteConnector, RouteKind,
    },
};
use spaceterm_host::{
    ipc::{IpcEndpoint, IpcTransport, PrivateIpcDirectory},
    ssh_tunnel::{
        AuthenticationMode, RemoteSocketPath, SshDestination, SshEnvironment, SshTunnel,
        SshTunnelCommand, TunnelAskPass,
    },
    transport::{
        EnrolledServerKey, MessageLimit, MessageTransport, TransportError, TransportRoute,
    },
    wss::WssTransport,
};
use std::{
    ffi::OsString,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

/// Local IPC endpoints admitted by explicit native discovery. A remote path,
/// saved string or server directory record alone cannot create this authority.
#[derive(Clone)]
pub struct LocalRouteAuthority {
    route: RouteDescription,
    endpoint: IpcEndpoint,
}
impl LocalRouteAuthority {
    pub fn new(endpoint: IpcEndpoint) -> Result<Self, ClientFailure> {
        let path = endpoint.path();
        let directory = path
            .parent()
            .and_then(|p| p.to_str())
            .ok_or_else(invalid_route)?;
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(invalid_route)?;
        let route = RouteDescription::new(RouteParts::Ipc {
            directory: directory.to_owned(),
            name: name.to_owned(),
        })?;
        Ok(Self { route, endpoint })
    }
    pub fn description(&self) -> &RouteDescription {
        &self.route
    }
}

/// One allocator belongs to the application's private per-process runtime
/// owner directory, never the server's runtime directory. Names do not repeat
/// within this allocator, including after a cancelled or failed tunnel start.
pub struct TunnelEndpoints {
    directory: PrivateIpcDirectory,
    prefix: [u8; 4],
    sequence: AtomicU64,
}
impl TunnelEndpoints {
    pub fn new(directory: PrivateIpcDirectory) -> Result<Self, ClientFailure> {
        let mut prefix = [0; 4];
        SystemRandom::new()
            .fill(&mut prefix)
            .map_err(|_| unavailable())?;
        Ok(Self {
            directory,
            prefix,
            sequence: AtomicU64::new(0),
        })
    }
    pub fn allocate(&self) -> Result<IpcEndpoint, ClientFailure> {
        let sequence = self
            .sequence
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_add(1))
            .map_err(|_| ClientFailure::new(ClientFailureClass::CounterExhausted))?;
        let mut bytes = [0; 12];
        bytes[..4].copy_from_slice(&self.prefix);
        bytes[4..].copy_from_slice(&sequence.to_be_bytes());
        self.directory
            .endpoint(&URL_SAFE_NO_PAD.encode(bytes))
            .map_err(classify)
    }
}

/// Trusted local OpenSSH configuration captured at process startup. No native
/// executable, configuration path, environment or AskPass capability is accepted
/// from a protocol message. Values have no diagnostic representation.
pub struct SshRouteConfiguration {
    executable: PathBuf,
    managed_config: PathBuf,
    home: PathBuf,
    environment: Vec<(OsString, OsString)>,
    endpoints: TunnelEndpoints,
    askpass: Option<Arc<dyn TunnelAskPass>>,
}
impl SshRouteConfiguration {
    pub fn new(
        executable: PathBuf,
        managed_config: PathBuf,
        home: PathBuf,
        environment: Vec<(OsString, OsString)>,
        endpoints: TunnelEndpoints,
        askpass: Option<Arc<dyn TunnelAskPass>>,
    ) -> Result<Self, ClientFailure> {
        // Validate captured environment now, not after a person's Retry action.
        SshEnvironment::new(home.clone(), environment.clone()).map_err(classify)?;
        if !executable.is_absolute() || !managed_config.is_absolute() {
            return Err(invalid_route());
        }
        Ok(Self {
            executable,
            managed_config,
            home,
            environment,
            endpoints,
            askpass,
        })
    }
    async fn open(
        &self,
        destination: &str,
        remote_socket: &str,
        limit: MessageLimit,
        foreground: Option<&CredentialUse>,
        cancellation: Option<&ForegroundCancellation>,
    ) -> Result<(Arc<dyn MessageTransport>, Arc<SshTunnel>), ClientFailure> {
        let askpass = self.foreground_askpass_with_cancellation(foreground, cancellation)?;
        let deadline = tokio::time::Instant::now() + DIAL_DEADLINE;
        let endpoint = self.endpoints.allocate()?;
        let command = SshTunnelCommand::new(
            self.executable.clone(),
            self.managed_config.clone(),
            SshDestination::new(destination).map_err(classify)?,
            endpoint,
            RemoteSocketPath::new(remote_socket).map_err(classify)?,
            if askpass.is_some() {
                AuthenticationMode::AskPass
            } else {
                AuthenticationMode::BatchMode
            },
        )
        .map_err(classify)?;
        let environment =
            SshEnvironment::new(self.home.clone(), self.environment.clone()).map_err(classify)?;
        let tunnel = Arc::new(
            SshTunnel::start(command, environment, askpass, DIAL_DEADLINE)
                .await
                .map_err(classify)?,
        );
        let connected =
            tokio::time::timeout_at(deadline, IpcTransport::connect(tunnel.endpoint(), limit))
                .await
                .map_err(|_| unavailable())
                .and_then(|result| result.map_err(classify));
        match connected {
            Ok(transport) => Ok((Arc::new(transport), tunnel)),
            Err(error) => {
                // No ConnectedRoute exists yet to own this cleanup. Return the
                // failure only after the tunnel's exact native completion.
                tunnel.cancel().await.map_err(classify)?;
                Err(error)
            }
        }
    }
}

/// Reuses the existing zeroized native broker. Both the credential generation and
/// the ordinary connection attempt must remain valid throughout the prompt lifetime.
struct ForegroundAskPass {
    broker: Arc<dyn TunnelAskPass>,
    authority: ForegroundPromptAuthority,
}
#[derive(Clone)]
struct ForegroundPromptAuthority {
    authorization: CredentialUse,
    cancellation: Option<ForegroundCancellation>,
}
impl ForegroundPromptAuthority {
    fn is_valid(&self) -> bool {
        self.cancellation
            .as_ref()
            .is_none_or(|cancellation| !cancellation.is_cancelled())
            && self
                .authorization
                .validate(self.authorization.scope())
                .is_ok()
    }
}
impl TunnelAskPass for ForegroundAskPass {
    fn handoff(&self) -> Result<spaceterm_host::ssh_tunnel::AskPassHandoff, TransportError> {
        if !self.authority.is_valid() {
            return Err(TransportError::PeerAccountRejected);
        }
        let authority = self.authority.clone();
        self.broker
            .handoff_authorized(Arc::new(move || authority.is_valid()))
    }
}
impl SshRouteConfiguration {
    fn foreground_askpass(
        &self,
        foreground: Option<&CredentialUse>,
    ) -> Result<Option<Arc<dyn TunnelAskPass>>, ClientFailure> {
        self.foreground_askpass_with_cancellation(foreground, None)
    }
    fn foreground_askpass_with_cancellation(
        &self,
        foreground: Option<&CredentialUse>,
        cancellation: Option<&ForegroundCancellation>,
    ) -> Result<Option<Arc<dyn TunnelAskPass>>, ClientFailure> {
        let Some(authorization) = foreground else {
            return Ok(None);
        };
        let authority = ForegroundPromptAuthority {
            authorization: authorization.clone(),
            cancellation: cancellation.cloned(),
        };
        if !authority.is_valid() {
            return Err(ClientFailure::new(ClientFailureClass::AuthorizationDenied));
        }
        if !matches!(
            authorization.purpose(),
            CredentialUsePurpose::Enrollment | CredentialUsePurpose::Ordinary
        ) || authorization.native_access() != NativeCredentialAccess::Foreground
        {
            return Err(ClientFailure::new(ClientFailureClass::AuthorizationDenied));
        }
        Ok(self.askpass.as_ref().map(|broker| {
            Arc::new(ForegroundAskPass {
                broker: broker.clone(),
                authority,
            }) as Arc<dyn TunnelAskPass>
        }))
    }
}

pub struct NativeRouteConnector {
    local: Vec<LocalRouteAuthority>,
    restricted_local_discovery: Option<crate::NativeClientProvider>,
    pub(crate) ssh: Option<SshRouteConfiguration>,
}
impl NativeRouteConnector {
    pub fn new(
        local: Vec<LocalRouteAuthority>,
        ssh: Option<SshRouteConfiguration>,
    ) -> Result<Self, ClientFailure> {
        if local.len() > 64 {
            return Err(ClientFailure::new(ClientFailureClass::ResourceLimit));
        }
        Ok(Self {
            local,
            restricted_local_discovery: None,
            ssh,
        })
    }
    /// Reconsult this fixed native account/build registration for each restricted
    /// IPC attempt. Ordinary and enrollment routes retain their existing admission.
    pub fn with_restricted_local_discovery(mut self, native: crate::NativeClientProvider) -> Self {
        self.restricted_local_discovery = Some(native);
        self
    }
}
impl RouteConnector for NativeRouteConnector {
    fn open<'a>(
        &'a self,
        target: &'a ConnectionTarget,
        purpose: ConnectionPurpose,
    ) -> ClientFuture<'a, ConnectedRoute> {
        Box::pin(async move {
            if !matches!(purpose, ConnectionPurpose::Ordinary) {
                return Err(ClientFailure::new(ClientFailureClass::AuthorizationDenied));
            }
            self.open_pairing_route(target.pin(), target.route(), None)
                .await
        })
    }
}
impl NativeRouteConnector {
    /// A deliberate ordinary reconnect reuses the saved pairing's revocable lease.
    /// It cannot enroll or acquire restricted-revocation prompt authority.
    pub async fn open_foreground_ordinary(
        &self,
        target: &ConnectionTarget,
        authorization: &CredentialUse,
        cancellation: ForegroundCancellation,
    ) -> Result<ConnectedRoute, ClientFailure> {
        use spaceterm_client::pairing::credentials::CredentialPurpose;
        authorization
            .validate(authorization.scope())
            .map_err(|_| ClientFailure::new(ClientFailureClass::AuthorizationDenied))?;
        if cancellation.is_cancelled()
            || authorization.purpose() != CredentialUsePurpose::Ordinary
            || authorization.native_access() != NativeCredentialAccess::Foreground
            || authorization.scope().parts().purpose
                != (CredentialPurpose::PairingAuthentication {
                    server: target.server(),
                })
            || authorization.scope().reference() != target.credential()
            || authorization.ordinary_pairing() != Some(&target.local_pairing()?)
        {
            return Err(ClientFailure::new(ClientFailureClass::AuthorizationDenied));
        }
        self.open_native_route(
            target.pin(),
            target.route(),
            None,
            Some(authorization),
            Some(&cancellation),
            false,
        )
        .await
    }
    pub(crate) async fn open_pairing_route(
        &self,
        pin: spaceterm_protocol::authentication::ServerPin,
        route: &RouteDescription,
        capture: Option<&spaceterm_client::journal::JournalSshCapture>,
    ) -> Result<ConnectedRoute, ClientFailure> {
        self.open_native_route(pin, route, capture, None, None, false)
            .await
    }
    pub(crate) async fn open_restricted_pairing_route(
        &self,
        pin: spaceterm_protocol::authentication::ServerPin,
        route: &RouteDescription,
        capture: Option<&spaceterm_client::journal::JournalSshCapture>,
    ) -> Result<ConnectedRoute, ClientFailure> {
        self.open_native_route(pin, route, capture, None, None, true)
            .await
    }
    pub(crate) async fn open_foreground_pairing_route(
        &self,
        pin: spaceterm_protocol::authentication::ServerPin,
        route: &RouteDescription,
        capture: Option<&spaceterm_client::journal::JournalSshCapture>,
        authorization: &CredentialUse,
    ) -> Result<ConnectedRoute, ClientFailure> {
        self.open_native_route(pin, route, capture, Some(authorization), None, false)
            .await
    }
    async fn open_native_route(
        &self,
        pin: spaceterm_protocol::authentication::ServerPin,
        route: &RouteDescription,
        capture: Option<&spaceterm_client::journal::JournalSshCapture>,
        foreground: Option<&CredentialUse>,
        cancellation: Option<&ForegroundCancellation>,
        restricted: bool,
    ) -> Result<ConnectedRoute, ClientFailure> {
        // Captured routes belong to pairing, not ordinary foreground attempts.
        // Never let that path discard an attempt's prompt cancellation fence.
        if capture.is_some() && cancellation.is_some() {
            return Err(ClientFailure::new(ClientFailureClass::AuthorizationDenied));
        }
        let limit =
            MessageLimit::new(spaceterm_protocol::limits::MAX_FRAME_BYTES).map_err(classify)?;
        let mut configuration_lease = None;
        let (transport, tunnel): (Arc<dyn MessageTransport>, Option<Arc<SshTunnel>>) = match route
            .parts()
        {
            RouteParts::Ipc { .. } => {
                // A failed renewal never falls back to a startup descriptor.
                // Remote routes do not enter native filesystem discovery.
                let renewed = if restricted && let Some(native) = &self.restricted_local_discovery {
                    Some(renew_restricted_local_authority(native.clone(), pin, route).await?)
                } else {
                    None
                };
                let authority = renewed
                    .as_ref()
                    .or_else(|| self.local.iter().find(|a| &a.route == route))
                    .ok_or_else(invalid_route)?;
                (
                    Arc::new(
                        tokio::time::timeout(
                            DIAL_DEADLINE,
                            IpcTransport::connect(&authority.endpoint, limit),
                        )
                        .await
                        .map_err(|_| unavailable())?
                        .map_err(classify)?,
                    ),
                    None,
                )
            }
            RouteParts::Wss {
                address,
                server_name,
            } => (
                Arc::new(
                    WssTransport::connect(
                        *address,
                        server_name,
                        EnrolledServerKey(pin.public_key),
                        limit,
                        DIAL_DEADLINE,
                    )
                    .await
                    .map_err(classify)?,
                ),
                None,
            ),
            RouteParts::Ssh {
                destination,
                remote_socket,
            } => {
                let configuration = self.ssh.as_ref().ok_or_else(invalid_route)?;
                let (transport, tunnel) = if let Some(capture) = capture {
                    let (transport, tunnel, lease) = configuration
                        .open_captured(destination, remote_socket, limit, capture, foreground)
                        .await?;
                    configuration_lease = Some(lease);
                    (transport, tunnel)
                } else {
                    configuration
                        .open(destination, remote_socket, limit, foreground, cancellation)
                        .await?
                };
                (transport, Some(tunnel))
            }
        };
        let facts = transport.peer_facts();
        let peer = PeerFacts {
            route: match facts.route {
                TransportRoute::PrivateIpc => RouteKind::PrivateIpc,
                TransportRoute::SecureWebSocket => RouteKind::SecureWebSocket,
            },
            local_account_verified: facts.local_uid.is_some(),
            pinned_server_key: facts.pinned_server_key.map(|key| key.0),
        };
        let owner = Arc::new(RouteOwner {
            transport,
            tunnel,
            _configuration: configuration_lease,
            aborted: AtomicBool::new(false),
            cleanup: tokio::sync::OnceCell::new(),
            cleanup_scheduled: AtomicBool::new(false),
        });
        Ok(ConnectedRoute {
            sender: Arc::new(Sender(Arc::clone(&owner))),
            receiver: Box::new(Receiver(owner)),
            peer,
        })
    }
}

async fn renew_restricted_local_authority(
    native: crate::NativeClientProvider,
    pin: spaceterm_protocol::authentication::ServerPin,
    route: &RouteDescription,
) -> Result<LocalRouteAuthority, ClientFailure> {
    let authority = tokio::task::spawn_blocking(move || {
        let registration = native
            .management()
            .supervisor()
            .discover()
            .map_err(|_| invalid_route())?
            .ok_or_else(unavailable)?;
        if registration.server_pin() != pin {
            return Err(ClientFailure::new(ClientFailureClass::PinMismatch));
        }
        let endpoint = registration.endpoint().map_err(|error| match error {
            spaceterm_host::account_service::ServiceFailure::Missing => unavailable(),
            _ => invalid_route(),
        })?;
        LocalRouteAuthority::new(endpoint)
    })
    .await
    .map_err(|_| unavailable())??;
    if authority.description() != route {
        return Err(ClientFailure::new(ClientFailureClass::StaleAuthority));
    }
    // Only this attempt owns the admitted directory identity. Later attempts
    // rediscover registration and revalidate endpoint provenance from scratch.
    Ok(authority)
}

struct RouteOwner {
    transport: Arc<dyn MessageTransport>,
    tunnel: Option<Arc<SshTunnel>>,
    _configuration: Option<Arc<tempfile::NamedTempFile>>,
    aborted: AtomicBool,
    cleanup: tokio::sync::OnceCell<Result<(), ClientFailure>>,
    cleanup_scheduled: AtomicBool,
}
impl RouteOwner {
    async fn close(&self) -> Result<(), ClientFailure> {
        self.aborted.store(true, Ordering::Release);
        *self
            .cleanup
            .get_or_init(|| async {
                let transport = self.transport.close().await.map_err(classify);
                let tunnel = if let Some(tunnel) = &self.tunnel {
                    tunnel.cancel().await.map(|_| ()).map_err(classify)
                } else {
                    Ok(())
                };
                transport.and(tunnel)
            })
            .await
    }

    fn abort(self: &Arc<Self>) {
        self.aborted.store(true, Ordering::Release);
        if self.cleanup.get().is_some() || self.cleanup_scheduled.swap(true, Ordering::AcqRel) {
            return;
        }
        // If the executor is already gone, dropping the last native transport
        // closes its descriptors, and SshTunnel's native supervisor still reaps.
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let owner = Arc::clone(self);
            runtime.spawn(async move {
                let _ = owner.close().await;
            });
        }
    }
}
struct Sender(Arc<RouteOwner>);
impl MessageSender for Sender {
    fn send<'a>(&'a self, message: &'a [u8]) -> ClientFuture<'a, ()> {
        Box::pin(async move {
            if self.0.aborted.load(Ordering::Acquire) {
                return Err(unavailable());
            }
            self.0.transport.send(message).await.map_err(classify)
        })
    }
    fn close(&self) -> ClientFuture<'_, ()> {
        Box::pin(self.0.close())
    }
    fn abort(&self) {
        self.0.abort();
    }
}
impl Drop for Sender {
    fn drop(&mut self) {
        self.0.abort();
    }
}
struct Receiver(Arc<RouteOwner>);
impl MessageReceiver for Receiver {
    fn receive(&mut self) -> ClientFuture<'_, Option<Vec<u8>>> {
        Box::pin(async {
            if self.0.aborted.load(Ordering::Acquire) {
                return Err(unavailable());
            }
            self.0.transport.recv().await.map_err(classify)
        })
    }
}

pub fn classify(error: TransportError) -> ClientFailure {
    ClientFailure::new(match error {
        TransportError::Unavailable
        | TransportError::Closed
        | TransportError::HandshakeExpired
        | TransportError::TunnelExited => ClientFailureClass::Unavailable,
        TransportError::UnsafeEndpoint => ClientFailureClass::UnsafeEndpoint,
        TransportError::PeerAccountRejected | TransportError::RemoteAccessDisabled => {
            ClientFailureClass::AuthorizationDenied
        }
        TransportError::TlsIdentityRejected => ClientFailureClass::PinMismatch,
        TransportError::MessageTooLarge => ClientFailureClass::ResourceLimit,
        _ => ClientFailureClass::InvalidPeer,
    })
}
fn unavailable() -> ClientFailure {
    ClientFailure::new(ClientFailureClass::Unavailable)
}
fn invalid_route() -> ClientFailure {
    ClientFailure::new(ClientFailureClass::UnsafeEndpoint)
}

mod bootstrap_ssh;
mod captured_ssh;
pub use bootstrap_ssh::NativeBootstrapHostFactory;

#[cfg(test)]
mod tests;
