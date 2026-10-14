//! Test composition only: deterministic keys, real authentication/codecs and core policy.
//! No discovery registration, native endpoint, credential mode or production server dependency.

mod domain;
mod journal;
mod pane;
mod transport;
pub use journal::MemoryJournal;
pub use pane::FakePaneConnection;

use crate::{
    api::*,
    failure::{ClientFailure, ClientFailureClass, exhausted, invalid_peer, resource_limit},
    operations::OperationPeer,
    ports::*,
    projection::stale,
    replica::*,
};
use ring::signature::{Ed25519KeyPair, KeyPair};
use spaceterm_core::{
    authority::*,
    control::*,
    identity::*,
    operation_ledger::{CanonicalOperationRequest, canonical::CanonicalCodec},
    operations::*,
    organization::*,
    pairing::*,
};
use spaceterm_protocol::{
    ProtocolError,
    authentication::{ServerPin, SigningKey},
    codec::Payload,
    negotiation::{CAPABILITY_CANONICAL_DOMAIN, CAPABILITY_NORMALIZED_CREATION_TOPOLOGY},
};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use tokio::sync::mpsc;

/// Explicit remote-account facts for portable Bootstrap consumers. This fixture does not select
/// a local OS adapter, inspect the test host or execute a service-manager operation.
pub fn bootstrap_host_facts(
    route: crate::pairing::bootstrap::BootstrapRoute,
    registration: crate::pairing::bootstrap::BootstrapRegistration,
) -> crate::pairing::bootstrap::BootstrapHostFacts {
    use crate::pairing::bootstrap::*;
    BootstrapHostFacts {
        route,
        account_uid: route.account_uid(),
        os: BootstrapOs::Linux,
        architecture: BootstrapArchitecture::X86_64,
        os_version: HostVersion::new(22, 4, 0),
        libc: BootstrapLibc::Glibc(HostVersion::new(2, 35, 0)),
        kernel: HostVersion::new(5, 15, 0),
        supervision: BootstrapSupervision::SystemdUser {
            version: 249,
            reachable: true,
        },
        registration,
        storage: BootstrapPrerequisite::Available,
        installation: BootstrapPrerequisite::Available,
        linger: BootstrapLinger::Enabled,
        sudo: BootstrapSudo::ResponseRequired,
        authorization_agent: true,
        background_items_allowed: true,
    }
}

pub struct DeterministicKey(Ed25519KeyPair);
impl DeterministicKey {
    pub fn new(seed: u8) -> Result<Self, ClientFailure> {
        Ok(Self(
            Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).map_err(|_| invalid_peer())?,
        ))
    }
}
impl SigningKey for DeterministicKey {
    fn public_key(&self) -> [u8; 32] {
        let mut key = [0; 32];
        key.copy_from_slice(self.0.public_key().as_ref());
        key
    }
    fn sign(&self, bytes: &[u8]) -> Result<[u8; 64], ProtocolError> {
        self.0
            .sign(bytes)
            .as_ref()
            .try_into()
            .map_err(|_| ProtocolError::Authentication)
    }
}
#[derive(Clone)]
pub enum FakeFault {
    Disconnect,
    OrganizationGap { revision: u64 },
    MaliciousEnvelope(Vec<u8>),
}
impl std::fmt::Debug for FakeFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disconnect => f.write_str("Disconnect"),
            Self::OrganizationGap { revision } => f
                .debug_struct("OrganizationGap")
                .field("revision", revision)
                .finish(),
            Self::MaliciousEnvelope(bytes) => f
                .debug_struct("MaliciousEnvelope")
                .field("length", &bytes.len())
                .finish(),
        }
    }
}
#[derive(Default)]
pub struct FakeFaults {
    pub drop_next_outcome: AtomicBool,
}

#[derive(Clone)]
pub struct FakeServer {
    pub(super) server: ServerId,
    pub(super) signer: Arc<DeterministicKey>,
    pub(super) device_signer: Arc<DeterministicKey>,
    pub(super) majors: Vec<u32>,
    domain: Arc<Mutex<domain::Domain>>,
    pub(super) clients: Arc<Mutex<BTreeMap<ClientId, OperationStreamId>>>,
    connections: Arc<Mutex<Vec<mpsc::Sender<FakeFault>>>>,
    slots: Arc<tokio::sync::Semaphore>,
    pub faults: Arc<FakeFaults>,
    received: Arc<Mutex<VecDeque<OperationBinding>>>,
    wire_majors: Arc<Mutex<Vec<u32>>>,
    next_connection: Arc<AtomicU64>,
}
impl FakeServer {
    pub fn new(seed: u8, majors: Vec<u32>) -> Result<Arc<Self>, ClientFailure> {
        if seed == 0 || majors.is_empty() || majors.iter().any(|major| ![1, 2].contains(major)) {
            return Err(invalid_peer());
        }
        let server = ServerId::new([seed; 16]);
        let value = Arc::new(Self {
            server,
            signer: Arc::new(DeterministicKey::new(seed)?),
            device_signer: Arc::new(DeterministicKey::new(99)?),
            majors,
            domain: Arc::new(Mutex::new(domain::Domain::new(server)?)),
            clients: Arc::new(Mutex::new(BTreeMap::new())),
            connections: Arc::new(Mutex::new(vec![])),
            slots: Arc::new(tokio::sync::Semaphore::new(16)),
            faults: Arc::new(FakeFaults::default()),
            received: Arc::new(Mutex::new(VecDeque::new())),
            wire_majors: Arc::new(Mutex::new(vec![])),
            next_connection: Arc::new(AtomicU64::new(0)),
        });
        value.add_client(1)?;
        Ok(value)
    }
    pub fn server(&self) -> ServerId {
        self.server
    }
    pub fn add_client(&self, local: u64) -> Result<PairedServerBinding, ClientFailure> {
        if !(1..=64).contains(&local) {
            return Err(resource_limit());
        }
        let client = ClientId::new(self.server, local).map_err(|_| invalid_peer())?;
        let stream = OperationStreamId::new(
            OperationStreamKey::new(self.server, local.checked_mul(2).ok_or_else(exhausted)?)
                .map_err(|_| invalid_peer())?,
            PairingId::new(self.server, 1).map_err(|_| invalid_peer())?,
            client,
        )
        .map_err(|_| invalid_peer())?;
        let mut clients = self.clients.lock().map_err(|_| invalid_peer())?;
        if let std::collections::btree_map::Entry::Vacant(entry) = clients.entry(client) {
            self.domain
                .lock()
                .map_err(|_| invalid_peer())?
                .register(stream)?;
            entry.insert(stream);
        }
        self.binding(local)
    }
    pub fn binding(&self, client: u64) -> Result<PairedServerBinding, ClientFailure> {
        Ok(PairedServerBinding::new(ConnectionTarget::new(
            ServerPin {
                identity: self.server.bytes(),
                public_key: self.signer.public_key(),
            },
            DeviceId::new([99; 16]),
            PairingId::new(self.server, 1).map_err(|_| invalid_peer())?,
            ClientId::new(self.server, client).map_err(|_| invalid_peer())?,
            RouteDescription::new(RouteParts::Ipc {
                directory: "/fixture/private".into(),
                name: "server".into(),
            })?,
            CredentialReference::new([9; 16]),
            LocalPairingGeneration::new(1).map_err(|_| invalid_peer())?,
        )?))
    }
    pub fn snapshot(&self) -> Result<OrganizationSnapshot, ClientFailure> {
        Ok(self
            .domain
            .lock()
            .map_err(|_| invalid_peer())?
            .reducer
            .snapshot())
    }
    pub fn replica(&self) -> Result<Arc<ReplicaSnapshot>, ClientFailure> {
        let snapshot = self.snapshot()?;
        let mut replica = Replica::new(
            self.server,
            Arc::new(OrganizationResidency::new(
                crate::limits::ClientLimits::default(),
            )?),
        );
        let token = replica.begin_resync(snapshot.epoch())?;
        replica.install_snapshot(token, snapshot)?;
        let value = replica
            .subscribe()
            .borrow()
            .retained()
            .cloned()
            .ok_or_else(invalid_peer)?;
        Ok(value)
    }
    pub fn connected_info(
        &self,
        client: u64,
        generation: u64,
        attempt: u64,
        major: u32,
    ) -> Result<ConnectedInfo, ClientFailure> {
        if !self.majors.contains(&major) || generation == 0 || attempt == 0 {
            return Err(invalid_peer());
        }
        let client = self.binding(client)?.target().client();
        let snapshot = self.snapshot()?;
        Ok(ConnectedInfo {
            server: self.server,
            pairing: PairingId::new(self.server, 1).map_err(|_| invalid_peer())?,
            client,
            epoch: snapshot.epoch(),
            binding: ConnectionBinding::new(client, ConnectionGeneration::new(generation)),
            major,
            limits: spaceterm_protocol::limits::NegotiatedLimits::default(),
            capabilities: ConnectionCapabilities::from_advertisement(&[
                CAPABILITY_NORMALIZED_CREATION_TOPOLOGY,
                CAPABILITY_CANONICAL_DOMAIN,
            ]),
            organization_revision: snapshot.state().revision,
            attempt,
        })
    }
    /// Supplies optional negotiated features to an otherwise unchanged fixture connection.
    pub fn connected_info_with_capabilities(
        &self,
        client: u64,
        generation: u64,
        attempt: u64,
        major: u32,
        capabilities: &[u32],
    ) -> Result<ConnectedInfo, ClientFailure> {
        let mut info = self.connected_info(client, generation, attempt, major)?;
        info.capabilities = ConnectionCapabilities::from_advertisement(capabilities);
        Ok(info)
    }
    pub fn command(
        &self,
        id: OperationId,
        command: Command,
    ) -> Result<OperationLookup, ClientFailure> {
        self.command_at(id, command, None)
    }
    fn command_at(
        &self,
        id: OperationId,
        command: Command,
        dispatch: Option<ConnectionBinding>,
    ) -> Result<OperationLookup, ClientFailure> {
        let request = CanonicalOperationRequest::new(id, command, &CanonicalCodec)
            .map_err(|_| invalid_peer())?;
        {
            let mut received = self.received.lock().map_err(|_| invalid_peer())?;
            if received.len() == 4096 {
                received.pop_front();
            }
            received.push_back(request.binding());
        }
        self.domain
            .lock()
            .map_err(|_| invalid_peer())?
            .command(&request, dispatch)
    }
    fn activate_connection(&self, binding: ConnectionBinding) -> Result<(), ClientFailure> {
        self.domain
            .lock()
            .map_err(|_| invalid_peer())?
            .activate(binding)
    }
    fn deactivate_connection(&self, binding: ConnectionBinding) {
        if let Ok(mut domain) = self.domain.lock() {
            domain.deactivate(binding);
        }
    }
    /// Test composition supplies local context facts separately from operation comparison fields.
    pub fn update_context(
        &self,
        binding: ConnectionBinding,
        pane: PaneId,
        context: ContextGeneration,
    ) -> Result<(), ClientFailure> {
        self.domain
            .lock()
            .map_err(|_| invalid_peer())?
            .update_context(binding, pane, context)
    }
    pub fn next_operation(&self, client: u64) -> Result<OperationId, ClientFailure> {
        let client = self.binding(client)?.target().client();
        let stream = *self
            .clients
            .lock()
            .map_err(|_| invalid_peer())?
            .get(&client)
            .ok_or_else(stale)?;
        self.domain
            .lock()
            .map_err(|_| invalid_peer())?
            .next_operation(stream)
    }

    pub fn retire_client_stream(&self, client: u64) -> Result<(), ClientFailure> {
        let id = self.binding(client)?.target().client();
        let stream = *self
            .clients
            .lock()
            .map_err(|_| invalid_peer())?
            .get(&id)
            .ok_or_else(stale)?;
        self.domain
            .lock()
            .map_err(|_| invalid_peer())?
            .retire_stream(stream)
    }
    pub fn event(
        &self,
        event: spaceterm_core::reducer::OrganizationEvent,
    ) -> Result<(), ClientFailure> {
        self.domain
            .lock()
            .map_err(|_| invalid_peer())?
            .event(&event)
    }
    /// Bounded history of the most recent 4096 submissions, including retries.
    pub fn received_operations(&self) -> Result<Vec<OperationBinding>, ClientFailure> {
        Ok(self
            .received
            .lock()
            .map_err(|_| invalid_peer())?
            .iter()
            .copied()
            .collect())
    }
    pub fn negotiated_majors(&self) -> Result<Vec<u32>, ClientFailure> {
        Ok(self.wire_majors.lock().map_err(|_| invalid_peer())?.clone())
    }
    pub fn fault(&self, fault: FakeFault) -> Result<(), ClientFailure> {
        let mut connections = self.connections.lock().map_err(|_| invalid_peer())?;
        connections.retain(|sender| !sender.is_closed());
        for sender in connections.iter() {
            sender
                .try_send(fault.clone())
                .map_err(|_| resource_limit())?;
        }
        Ok(())
    }
    /// Visibly separate from normal domain policy. This bypasses envelope validation.
    pub fn malicious_envelope(&self, bytes: Vec<u8>) -> Result<(), ClientFailure> {
        if bytes.len() > spaceterm_protocol::limits::MAX_FRAME_BYTES {
            return Err(resource_limit());
        }
        self.fault(FakeFault::MaliciousEnvelope(bytes))
    }
}
impl CredentialSource for FakeServer {
    fn signing_access<'a>(
        &'a self,
        target: &'a ConnectionTarget,
        purpose: ConnectionPurpose,
    ) -> ClientFuture<'a, SigningAccess> {
        Box::pin(async move {
            if purpose != ConnectionPurpose::Ordinary
                || target.pin() != self.binding(target.client().local())?.target().pin()
                || !self
                    .clients
                    .lock()
                    .map_err(|_| invalid_peer())?
                    .contains_key(&target.client())
            {
                return Err(stale());
            }
            Ok(SigningAccess(self.device_signer.clone()))
        })
    }
}
impl OperationPeer for FakeServer {
    fn validate_submission(
        &self,
        binding: &PairedServerBinding,
        command: &Command,
        workspace: Option<WorkspaceId>,
    ) -> Result<(), ClientFailure> {
        if binding != &self.binding(binding.target().client().local())?
            || command.server() != self.server
            || workspace.is_some_and(|workspace| {
                !self.snapshot().is_ok_and(|snapshot| {
                    snapshot
                        .state()
                        .workspaces
                        .iter()
                        .any(|record| record.id == workspace)
                })
            })
        {
            return Err(stale());
        }
        Ok(())
    }
    fn submit<'a>(&'a self, request: &'a CanonicalOperationRequest) -> ClientFuture<'a, ()> {
        Box::pin(async move {
            self.command(request.binding().id, request.command().clone())?;
            Ok(())
        })
    }
    fn lookup(&self, binding: OperationBinding) -> ClientFuture<'_, OperationLookup> {
        Box::pin(async move {
            self.domain
                .lock()
                .map_err(|_| invalid_peer())?
                .lookup(binding)
        })
    }
    fn acknowledge(&self, stream: OperationStreamId, through: u64) -> ClientFuture<'_, ()> {
        Box::pin(async move {
            self.domain
                .lock()
                .map_err(|_| invalid_peer())?
                .acknowledge(stream, through)
        })
    }
}
