//! Client correlation and authenticated codec adaptation. Generic domain
//! publication conversion is supplied by server-core, not duplicated here.

use crate::{
    api::{ConnectionCapabilities, ConnectionTarget},
    failure::{ClientFailure, ClientFailureClass, invalid_peer},
    ports::{
        ClientFuture, ConnectedRoute, ConnectionDelegate, ConnectionPurpose, CredentialSource,
        ProtocolChannel, RouteKind,
    },
    replica::OrganizationCursor,
};
use spaceterm_core::{
    authority::{ConnectionBinding, ConnectionGeneration},
    identity::ClientId,
};
use spaceterm_protocol::{
    authentication::{ClientHandshake, Established, HandshakeConfig, Principal, Purpose},
    codec::Payload,
    connection::AuthenticatedConnection,
    negotiation::{
        CAPABILITY_CANONICAL_DOMAIN, CAPABILITY_NORMALIZED_CREATION_TOPOLOGY,
        CAPABILITY_ORGANIZATION_COMPARISONS,
    },
    wire::{common, v1, v2},
};

pub(crate) struct ProtocolSession {
    pub codec: std::sync::Arc<std::sync::Mutex<AuthenticatedConnection>>,
    pub major: u32,
    pub epoch: u64,
    pub limits: spaceterm_protocol::limits::NegotiatedLimits,
    pub capabilities: ConnectionCapabilities,
    pub route: ConnectedRoute,
    pub writer: crate::connection::writer::Writer,
    delegate: Option<(
        std::sync::Arc<dyn ConnectionDelegate>,
        crate::api::ConnectedInfo,
    )>,
}

fn local_identity(value: u64) -> [u8; 16] {
    let mut bytes = [0; 16];
    bytes[8..].copy_from_slice(&value.to_be_bytes());
    bytes
}
fn identity(bytes: [u8; 16]) -> common::Identity {
    common::Identity {
        value: bytes.to_vec(),
    }
}
fn client_entity(client: ClientId) -> common::Entity {
    common::Entity {
        server: Some(identity(client.server().bytes())),
        local_id: client.local(),
    }
}

impl ProtocolSession {
    pub async fn authenticate(
        target: &ConnectionTarget,
        mut route: ConnectedRoute,
        credentials: &dyn CredentialSource,
        additional_capabilities: &[u32],
        egress: std::sync::Arc<crate::registry::EgressBudget>,
    ) -> Result<Self, ClientFailure> {
        match route.peer.route {
            RouteKind::PrivateIpc if !route.peer.local_account_verified => {
                return Err(ClientFailure::new(ClientFailureClass::AuthorizationDenied));
            }
            RouteKind::SecureWebSocket
                if route.peer.pinned_server_key != Some(target.pin().public_key) =>
            {
                return Err(ClientFailure::new(ClientFailureClass::PinMismatch));
            }
            _ => {}
        }
        let signer = credentials
            .signing_access(target, ConnectionPurpose::Ordinary)
            .await?;
        let principal = Principal {
            server: target.server().bytes(),
            device: target.device().bytes(),
            pairing: local_identity(target.pairing().local()),
            client: local_identity(target.client().local()),
        };
        let mut capabilities = vec![
            CAPABILITY_NORMALIZED_CREATION_TOPOLOGY,
            CAPABILITY_CANONICAL_DOMAIN,
            CAPABILITY_ORGANIZATION_COMPARISONS,
            spaceterm_protocol::negotiation::CAPABILITY_SERVER_MACHINE_FACTS,
        ];
        capabilities.extend_from_slice(additional_capabilities);
        capabilities.sort_unstable();
        capabilities.dedup();
        let config = HandshakeConfig {
            capabilities,
            ..HandshakeConfig::default()
        };
        let (handshake, hello) =
            ClientHandshake::begin(target.pin(), principal, Purpose::Ordinary, config, signer.0)?;
        route.sender.send(&hello).await?;
        let reply = route.receiver.receive().await?.ok_or_else(unavailable)?;
        let (ready, proof) = handshake.receive_server(&reply)?;
        route.sender.send(&proof).await?;
        let reply = route.receiver.receive().await?.ok_or_else(unavailable)?;
        let Established::Ordinary(codec) = ready.receive_ready(&reply)? else {
            return Err(invalid_peer());
        };
        if codec.epoch() == 0 || codec.principal() != principal {
            return Err(invalid_peer());
        }
        if !codec.capabilities().contains(&CAPABILITY_CANONICAL_DOMAIN)
            || !codec
                .capabilities()
                .contains(&CAPABILITY_NORMALIZED_CREATION_TOPOLOGY)
        {
            return Err(ClientFailure::new(ClientFailureClass::MissingCapability));
        }
        let codec = std::sync::Arc::new(std::sync::Mutex::new(codec));
        let writer = crate::connection::writer::Writer::new(
            std::sync::Arc::clone(&codec),
            std::sync::Arc::clone(&route.sender),
            codec.lock().map_err(|_| invalid_peer())?.limits(),
            egress,
        );
        let facts = codec.lock().map_err(|_| invalid_peer())?;
        let (major, epoch, limits, capabilities) = (
            facts.major(),
            facts.epoch(),
            facts.limits(),
            ConnectionCapabilities::from_advertisement(facts.capabilities()),
        );
        drop(facts);
        Ok(Self {
            major,
            epoch,
            limits,
            capabilities,
            codec,
            route,
            writer,
            delegate: None,
        })
    }

    pub fn bind_delegate(
        &mut self,
        delegate: std::sync::Arc<dyn ConnectionDelegate>,
        info: crate::api::ConnectedInfo,
    ) {
        self.delegate = Some((delegate, info));
    }
    pub async fn send_payload(&mut self, payload: &Payload) -> Result<(), ClientFailure> {
        self.writer.enqueue(payload, None)
    }
    pub async fn receive_payload(&mut self) -> Result<Payload, ClientFailure> {
        if let Some(error) = *self.writer.failure.borrow() {
            return Err(error);
        }
        let bytes = tokio::select! {
            result = self.route.receiver.receive() => result?.ok_or_else(unavailable)?,
            _ = self.writer.failure.changed() => return Err(self.writer.failure.borrow().unwrap_or_else(unavailable)),
        };
        self.codec
            .lock()
            .map_err(|_| invalid_peer())?
            .decode(&bytes)
            .map_err(Into::into)
    }
}
impl ProtocolChannel for ProtocolSession {
    fn send<'a>(&'a mut self, message: &'a Payload) -> ClientFuture<'a, ()> {
        Box::pin(async move { self.writer.enqueue(message, self.delegate.clone()) })
    }
    fn receive(&mut self) -> ClientFuture<'_, Payload> {
        Box::pin(self.receive_payload())
    }
}

pub(crate) fn unavailable() -> ClientFailure {
    ClientFailure::new(ClientFailureClass::Unavailable)
}

pub(crate) fn organization_query(major: u32, request: u64) -> Result<Payload, ClientFailure> {
    macro_rules! query {
        ($p:ident, $variant:ident) => {
            Payload::$variant($p::Message {
                kind: Some($p::message::Kind::OrganizationQuery(
                    $p::OrganizationQuery {
                        known_revision: 0,
                        known_epoch: 0,
                        request,
                        request_generation: request,
                        continuation: vec![],
                        part: None,
                    },
                )),
            })
        };
    }
    Ok(match major {
        1 => query!(v1, Major1),
        2 => query!(v2, Major2),
        _ => return Err(invalid_peer()),
    })
}

pub(crate) fn reconciliation(
    major: u32,
    request: u64,
    client: ClientId,
    cursor: OrganizationCursor,
) -> Result<Payload, ClientFailure> {
    macro_rules! query {
        ($p:ident, $variant:ident) => {
            Payload::$variant($p::Message {
                kind: Some($p::message::Kind::Reconciliation($p::Reconciliation {
                    request,
                    request_generation: request,
                    server: Some(identity(cursor.server.bytes())),
                    epoch: cursor.epoch.get(),
                    client: Some(client_entity(client)),
                    connection_generation: 0,
                    organization_revision: cursor.revision.get(),
                    acknowledged: false,
                    grid_limits: None,
                })),
            })
        };
    }
    Ok(match major {
        1 => query!(v1, Major1),
        2 => query!(v2, Major2),
        _ => return Err(invalid_peer()),
    })
}

pub(crate) fn reconciliation_binding(
    payload: &Payload,
    request: u64,
    client: ClientId,
    cursor: OrganizationCursor,
) -> Result<Option<ConnectionBinding>, ClientFailure> {
    macro_rules! check {
        ($message:expr, $p:ident) => {
            if let Some($p::message::Kind::Reconciliation(reply)) = &$message.kind {
                if reply.request != request || reply.request_generation != request {
                    return Ok(None);
                }
                if !reply.acknowledged
                    || reply
                        .server
                        .as_ref()
                        .is_none_or(|s| s.value != cursor.server.bytes())
                    || reply.epoch != cursor.epoch.get()
                    || reply.organization_revision > cursor.revision.get()
                    || reply.client.as_ref() != Some(&client_entity(client))
                    || reply.connection_generation == 0
                {
                    return Err(invalid_peer());
                }
                if reply.organization_revision < cursor.revision.get() {
                    return Ok(None);
                }
                return Ok(Some(ConnectionBinding::new(
                    client,
                    ConnectionGeneration::new(reply.connection_generation),
                )));
            }
        };
    }
    match payload {
        Payload::Major1(message) => check!(message, v1),
        Payload::Major2(message) => check!(message, v2),
    }
    Ok(None)
}

pub(crate) fn is_organization(payload: &Payload) -> bool {
    macro_rules! check {
        ($message:expr, $p:ident) => {
            matches!(
                $message.kind,
                Some(
                    $p::message::Kind::OrganizationSnapshot(_)
                        | $p::message::Kind::OrganizationDelta(_)
                )
            )
        };
    }
    match payload {
        Payload::Major1(message) => check!(message, v1),
        Payload::Major2(message) => check!(message, v2),
    }
}

pub(crate) fn is_resync(payload: &Payload) -> bool {
    match payload {
        Payload::Major1(message) => {
            matches!(message.kind, Some(v1::message::Kind::OrganizationResync(_)))
        }
        Payload::Major2(message) => {
            matches!(message.kind, Some(v2::message::Kind::OrganizationResync(_)))
        }
    }
}

/// Stop is a qualified authenticated management fact, not endpoint reachability.
/// The generic status converter remains server-core-owned.
pub(crate) fn confirmed_stopped(
    payload: &Payload,
    info: crate::api::ConnectedInfo,
) -> Result<bool, ClientFailure> {
    macro_rules! check {
        ($message:expr,$p:ident) => {{
            let status = match &$message.kind {
                Some($p::message::Kind::ManagementStatus(status)) => Some(status),
                Some($p::message::Kind::StatusPublication(publication))
                    if !publication.resync_required =>
                {
                    publication.status.as_ref()
                }
                _ => None,
            };
            if let Some(status) = status
                && status.state == $p::ServerState::Stopped as i32
                && status.native_verified
            {
                if status
                    .server
                    .as_ref()
                    .is_none_or(|server| server.value != info.server.bytes())
                    || status.epoch != info.epoch.get()
                    || status.status_revision == 0
                    || status.failure.is_some()
                {
                    return Err(invalid_peer());
                }
                return Ok(true);
            }
        }};
    }
    match payload {
        Payload::Major1(message) => check!(message, v1),
        Payload::Major2(message) => check!(message, v2),
    }
    Ok(false)
}
