#![allow(
    dead_code,
    reason = "connection fixtures are shared by independently scoped integration suites"
)]
pub mod peer;
use ring::signature::{Ed25519KeyPair, KeyPair};
use spaceterm_client::{
    api::{ConnectedInfo, ConnectionTarget, PairedServerBinding, RouteDescription, RouteParts},
    failure::{ClientFailure, ClientFailureClass},
    limits::ClientLimits,
    ports::*,
    replica::{OrganizationCursor, PublicationStamp, ReplicaSnapshot, SnapshotPart},
};
use spaceterm_core::{
    authority::{FieldVersion, OrganizationRevision, ServerEpoch},
    identity::{ClientId, DeviceId, PairingId, ServerId},
    organization::{
        OrganizationSnapshot, OrganizationState, ServerSettingsRecord, Versioned,
        WorktreeLocationTemplate,
    },
    organization_retained::{OrganizationRetainedFacts, OrganizationRetainedParts},
    pairing::{CredentialReference, LocalPairingGeneration},
    remote_access::RemoteAccessPolicy,
};
use spaceterm_protocol::{
    ProtocolError,
    authentication::{ServerPin, SigningKey},
    codec::Payload,
    wire::{v1, v2},
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

pub struct TestKey(Ed25519KeyPair);
impl TestKey {
    pub fn new(seed: u8) -> Self {
        Self(Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).unwrap())
    }
}
impl SigningKey for TestKey {
    fn public_key(&self) -> [u8; 32] {
        self.0.public_key().as_ref().try_into().unwrap()
    }
    fn sign(&self, bytes: &[u8]) -> Result<[u8; 64], ProtocolError> {
        Ok(self.0.sign(bytes).as_ref().try_into().unwrap())
    }
}
pub fn binding(server: u8) -> PairedServerBinding {
    let id = ServerId::new([server; 16]);
    PairedServerBinding::new(
        ConnectionTarget::new(
            ServerPin {
                identity: id.bytes(),
                public_key: TestKey::new(server).public_key(),
            },
            DeviceId::new([99; 16]),
            PairingId::new(id, 1).unwrap(),
            ClientId::new(id, 1).unwrap(),
            RouteDescription::new(RouteParts::Ipc {
                directory: "/private/local".into(),
                name: "server".into(),
            })
            .unwrap(),
            CredentialReference::new([9; 16]),
            LocalPairingGeneration::new(1).unwrap(),
        )
        .unwrap(),
    )
}
pub fn snapshot(server: ServerId, epoch: u64, revision: u64) -> OrganizationSnapshot {
    let retained = OrganizationRetainedFacts::new(OrganizationRetainedParts {
        server,
        workspace_names: vec![],
        creation_defaults: vec![],
        pane_generations: vec![],
        tab_membership_versions: vec![],
        repository_inventory_versions: vec![],
        worktree_inventory: vec![],
        remote_access: Versioned {
            version: FieldVersion::new(1),
            value: RemoteAccessPolicy::disabled(server),
        },
    })
    .unwrap();
    OrganizationSnapshot::new(
        ServerEpoch::new(epoch),
        OrganizationState {
            server,
            revision: OrganizationRevision::new(revision),
            settings: ServerSettingsRecord {
                worktree_location: Versioned {
                    version: FieldVersion::new(1),
                    value: WorktreeLocationTemplate::new("{branch}".into()).unwrap(),
                },
            },
            workspaces: vec![],
            tabs: vec![],
            panes: vec![],
            repositories: vec![],
            worktrees: vec![],
            retained: Some(retained),
        },
        vec![],
    )
    .unwrap()
}

pub struct Journal(pub Vec<PairedServerBinding>);
impl ConnectionJournal for Journal {
    fn recover_bindings(&self) -> ClientFuture<'_, Vec<PairedServerBinding>> {
        Box::pin(async { Ok(self.0.clone()) })
    }
    fn ordinary_use_allowed<'a>(
        &'a self,
        binding: &'a PairedServerBinding,
    ) -> ClientFuture<'a, bool> {
        Box::pin(async move { Ok(self.0.contains(binding)) })
    }
}
pub struct Credentials;
impl CredentialSource for Credentials {
    fn signing_access<'a>(
        &'a self,
        _target: &'a ConnectionTarget,
        _purpose: ConnectionPurpose,
    ) -> ClientFuture<'a, SigningAccess> {
        Box::pin(async { Ok(SigningAccess(Arc::new(TestKey::new(99)))) })
    }
}
pub struct Decoder;
impl OrganizationDecoder for Decoder {
    fn decode(&self, payload: Payload) -> Result<OrganizationPublication, ClientFailure> {
        macro_rules! decode {
            ($message:expr, $p:ident) => {
                match $message.kind {
                    Some($p::message::Kind::OrganizationSnapshot(s)) => {
                        let server = ServerId::new(s.server.unwrap().value.try_into().unwrap());
                        let part = s.part.unwrap();
                        let state = snapshot(server, s.epoch, s.revision).state().clone();
                        Ok(OrganizationPublication::Snapshot(Box::new(SnapshotPart {
                            stamp: PublicationStamp {
                                request: part.request,
                                request_generation: part.request_generation,
                                transfer: part.transfer,
                                index: part.index,
                                total: part.total,
                                cursor: OrganizationCursor {
                                    server,
                                    epoch: ServerEpoch::new(s.epoch),
                                    revision: OrganizationRevision::new(s.revision),
                                },
                            },
                            state,
                            runtime: vec![],
                        })))
                    }
                    Some($p::message::Kind::OrganizationDelta(d)) => {
                        let server = ServerId::new(d.server.unwrap().value.try_into().unwrap());
                        Ok(OrganizationPublication::Delta(
                            spaceterm_core::organization::OrganizationDelta::new(
                                server,
                                ServerEpoch::new(d.epoch),
                                OrganizationRevision::new(d.base_revision),
                                OrganizationRevision::new(d.revision),
                                vec![],
                                vec![],
                            )
                            .unwrap(),
                        ))
                    }
                    _ => Err(ClientFailure::new(ClientFailureClass::InvalidPeer)),
                }
            };
        }
        match payload {
            Payload::Major1(m) => decode!(m, v1),
            Payload::Major2(m) => decode!(m, v2),
        }
    }
}
#[derive(Default)]
pub struct Delegate {
    pub invalidations: AtomicUsize,
    pub reconciliations: AtomicUsize,
    pub messages: Mutex<Vec<u32>>,
}
impl ConnectionDelegate for Delegate {
    fn invalidate(&self, _binding: &PairedServerBinding, _reason: ConnectionInvalidation) {
        self.invalidations.fetch_add(1, Ordering::SeqCst);
    }
    fn reconcile<'a>(
        &'a self,
        _info: ConnectedInfo,
        _replica: &'a ReplicaSnapshot,
        _channel: &'a mut dyn ProtocolChannel,
    ) -> ClientFuture<'a, ()> {
        Box::pin(async {
            self.reconciliations.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
    fn publication(
        &self,
        _info: ConnectedInfo,
        _replica: &ReplicaSnapshot,
    ) -> Result<(), ClientFailure> {
        Ok(())
    }
    fn admit(
        &self,
        _info: ConnectedInfo,
        _message: &Payload,
    ) -> Result<OutboundClass, ClientFailure> {
        Ok(OutboundClass::Ordinary)
    }
    fn receive<'a>(
        &'a self,
        _info: ConnectedInfo,
        message: Payload,
        _channel: &'a mut dyn ProtocolChannel,
    ) -> ClientFuture<'a, ()> {
        Box::pin(async move {
            self.messages.lock().unwrap().push(message.major());
            Ok(())
        })
    }
}
pub fn services(
    bindings: Vec<PairedServerBinding>,
    routes: Arc<dyn RouteConnector>,
) -> ClientServices {
    ClientServices {
        routes,
        journal: Arc::new(Journal(bindings)),
        credentials: Arc::new(Credentials),
        organization: Arc::new(Decoder),
        delegate: Arc::new(Delegate::default()),
        discovery: None,
        limits: ClientLimits::default(),
        additional_capabilities: vec![],
    }
}
pub async fn settle() {
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }
}
