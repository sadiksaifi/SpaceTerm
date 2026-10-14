//! Real file identity, SQLite pairing, IPC and pinned WSS. Native runtime and
//! owner-authorization fakes stay in this test composition; no service is installed.
#![cfg(any(target_os = "macos", target_os = "linux"))]

#[path = "support/job_a.rs"]
mod native;

use ring::signature::{Ed25519KeyPair, KeyPair};
use spaceterm_core::{
    identity::*, management::*, operation_ledger::canonical::CanonicalCodec,
    operation_ledger_contracts::CanonicalOperationRequest, operations::*, organization::*,
    pairing::*, remote_access::RemoteAccessPolicy,
};
use spaceterm_host::{
    account_service::test_support::AccountServiceFixture,
    ipc::IpcTransport,
    native_deployment::contracts::NativeRequestId,
    organization_store::{
        SqliteOrganizationStore, StoreIdentity, StoreLimits, bootstrap::*, migrations, schema,
    },
    startup::{BuildIdentity, DesiredServerState},
    storage::Database,
    transport::{EnrolledServerKey, MessageLimit, MessageTransport, TransportError},
    wss::WssTransport,
};
use spaceterm_protocol::{
    ProtocolError,
    authentication::{self, SigningKey},
    codec::Payload,
    convert::{management, operations},
    pairing_adapter::*,
    wire::{v1, v2},
};
use spaceterm_server::{
    account_startup::AccountStartup,
    adapters::{core::CorePorts, store::production_services},
    api::*,
    contracts::*,
    listeners::{ProductionListenerConfig, ProductionListenerPorts, production_listener_factories},
    pairing::{
        management::AccountOwnerEnrollmentPort, protocol_lookup::PairingProtocolLookup,
        service::PairingService,
    },
    service::RecoveredServer,
    shutdown::TrustedShutdownRequest,
};
use std::{
    fs,
    net::{Ipv4Addr, SocketAddr},
    os::unix::fs::PermissionsExt,
    sync::Arc,
    time::Duration,
};

struct DeviceKey(Ed25519KeyPair);
impl DeviceKey {
    fn new(seed: u8) -> Arc<Self> {
        Arc::new(Self(
            Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).unwrap(),
        ))
    }
}
impl SigningKey for DeviceKey {
    fn public_key(&self) -> [u8; 32] {
        self.0.public_key().as_ref().try_into().unwrap()
    }
    fn sign(&self, bytes: &[u8]) -> Result<[u8; 64], ProtocolError> {
        Ok(self.0.sign(bytes).as_ref().try_into().unwrap())
    }
}
struct Authority(authentication::ServerPin);
impl StartupAuthorityPort for Authority {
    fn validate_startup(&self) -> Result<ServerId, ServerFailure> {
        Ok(ServerId::new(self.0.identity))
    }
    fn validate_credentials(
        &self,
        key: &dyn ServerCredentialPort,
        server: ServerId,
    ) -> Result<(), ServerFailure> {
        assert_eq!(server.bytes(), self.0.identity);
        assert_eq!(key.public_key(), self.0.public_key);
        Ok(())
    }
}
struct Owner;
impl AccountOwnerEnrollmentPort for Owner {
    fn validate_owner_request(
        &self,
        _: &NativeRequestId,
        _: RequestDigest,
        server: ServerId,
        proposal: &EnrollmentProposal,
    ) -> Result<(), PairingFailure> {
        if proposal.parts().server == server {
            Ok(())
        } else {
            Err(PairingFailure::IdentityMismatch)
        }
    }
    fn revalidate_owner_issuance(
        &self,
        _: &NativeRequestId,
        _: RequestDigest,
        _: ServerId,
        _: RequestDigest,
    ) -> Result<(), PairingFailure> {
        Ok(())
    }
}

struct DelayedStore {
    inner: SqliteOrganizationStore,
    gate: Arc<std::sync::Mutex<Option<Arc<native::StoreGate>>>>,
}
impl spaceterm_core::storage::OrganizationStore for DelayedStore {
    fn recover(
        &mut self,
    ) -> Result<spaceterm_core::storage::RecoveredState, spaceterm_core::storage::StoreFailure>
    {
        self.inner.recover()
    }
    fn commit(
        &mut self,
        transaction: &spaceterm_core::storage::DurableTransaction,
    ) -> spaceterm_core::storage::CommitDisposition {
        let gate = self.gate.lock().unwrap().take();
        if let Some(gate) = gate {
            gate.block();
        }
        self.inner.commit(transaction)
    }
}

struct Fixture {
    write_gate: Arc<std::sync::Mutex<Option<Arc<native::StoreGate>>>>,
    _root: tempfile::TempDir,
    registration: spaceterm_host::account_service::RegisteredServer,
    identity: Arc<spaceterm_host::server_keys::ServerIdentity>,
    server: RecoveredServer,
    service: PairingService,
    ports: Arc<ProductionListenerPorts>,
    stop: Arc<native::Host>,
    owner: Option<Arc<spaceterm_host::account_service::ServiceSupervisor>>,
}
impl Fixture {
    async fn new() -> Self {
        Self::new_registered(false).await
    }
    async fn new_registered(approved: bool) -> Self {
        Self::with_native_status(approved, None).await
    }
    async fn with_native_status(
        approved: bool,
        observer: Option<Arc<dyn spaceterm_server::listeners::NativeStatusObserver>>,
    ) -> Self {
        #[cfg(target_os = "macos")]
        let parent = std::path::PathBuf::from("/private/tmp");
        // /var/tmp is persistent temporary storage, separate from build output
        // on the Linux test host. /tmp may be tmpfs and is not admitted by SQLite.
        #[cfg(target_os = "linux")]
        let parent = std::path::PathBuf::from("/var/tmp");
        let root = tempfile::tempdir_in(parent).unwrap();
        let native_account =
            AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registration = native_account
            .initialize_with_installation(
                DesiredServerState::Running,
                approved.then(|| spaceterm_host::account_service::InstallationStatus {
                    installation: spaceterm_host::startup::InstallationId(
                        std::num::NonZeroU64::MIN,
                    ),
                    version: spaceterm_server::distribution_metadata::version().unwrap(),
                    artifact_sha256: [1; 32],
                }),
            )
            .unwrap();
        let pin = registration.server_pin();
        let identity = StoreIdentity {
            server: ServerId::new(pin.identity),
            account: registration.key(),
            server_public_key: pin.public_key,
        };
        let home = ServerDirectory::new(
            identity.server,
            root.path().as_os_str().as_encoded_bytes().to_vec(),
        )
        .unwrap();
        assert_eq!(
            fs::metadata(registration.organization_directory())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        let database = Database::open_for_application(
            registration.organization_directory(),
            migrations(),
            schema::APPLICATION_ID,
        )
        .unwrap();
        let services = production_services(identity, home.clone()).unwrap();
        match SqliteOrganizationStore::initialize_new(
            database,
            NewStoreSeed {
                identity,
                origin: NewServerOrigin::LocalHeadless,
            },
            StoreLimits::production(),
            services,
        ) {
            InitializationDisposition::Durable { store, .. } => drop(store),
            other => panic!("store initialization {other:?}"),
        }
        let account = AccountStartup::open(registration)
            .unwrap()
            .open_store(home.clone())
            .unwrap();
        let mut boundary = native::Fixture::new();
        let stop = boundary.host.clone();
        let ports = observer.map_or_else(
            ProductionListenerPorts::new,
            ProductionListenerPorts::with_native_status,
        );
        boundary.config.host.authority = Arc::new(Authority(pin));
        boundary.config.core =
            CorePorts::production(identity, home, boundary.dependencies.clock.clone()).unwrap();
        boundary.config.outcomes = ports.outcomes.clone();
        boundary.config.extensions = ports.extensions(boundary.config.extensions);
        boundary.dependencies.credentials = account.credentials.clone();
        let write_gate = Arc::new(std::sync::Mutex::new(None));
        boundary.dependencies.store = Box::new(DelayedStore {
            inner: account.store,
            gate: write_gate.clone(),
        });
        let server = RecoveredServer::recover(boundary.config, boundary.dependencies)
            .await
            .unwrap();
        let owner = approved.then(|| Arc::new(native_account.reopen()));
        let owner_port: Arc<dyn AccountOwnerEnrollmentPort> = match &owner {
            Some(supervisor) => Arc::new(
                spaceterm_server::account_startup::AccountOwnerEnrollment::new(supervisor.clone()),
            ),
            None => Arc::new(Owner),
        };
        let service = PairingService::new(
            Arc::new(server.handle()),
            account.credentials.clone(),
            Arc::new(CanonicalCodec),
            owner_port,
            pin,
        )
        .unwrap();
        Self {
            write_gate,
            _root: root,
            registration: account.registration,
            identity: account.credentials,
            server,
            service,
            ports,
            stop,
            owner,
        }
    }
    async fn enroll(
        &self,
        major: u32,
    ) -> (
        PairingClientStreams,
        authentication::Principal,
        Arc<DeviceKey>,
    ) {
        self.enroll_with_seed(major, 71).await
    }
    async fn enroll_with_seed(
        &self,
        major: u32,
        seed: u8,
    ) -> (
        PairingClientStreams,
        authentication::Principal,
        Arc<DeviceKey>,
    ) {
        let device = DeviceKey::new(seed);
        let auth = DeviceKey::new(seed + 1);
        let proposal = EnrollmentProposal::new(EnrollmentProposalParts {
            server: ServerId::new(self.identity.pin().identity),
            device: DeviceId::new([seed; 16]),
            keys: PairingKeys::new(PairingKeysParts {
                device_identity_public_key: device.public_key(),
                pairing_authentication_public_key: auth.public_key(),
            })
            .unwrap(),
            claim_nonce: [seed; 32],
        })
        .unwrap();
        let (proof_device, proof_auth) =
            VerifiedDeviceClaim::proofs(&proposal, device.as_ref(), auth.as_ref()).unwrap();
        let (owner_request, owner_digest) = match &self.owner {
            Some(supervisor) => {
                let request = supervisor.allocate_request().unwrap();
                let owner = spaceterm_server::account_startup::AccountOwnerEnrollment::new(
                    supervisor.clone(),
                );
                let digest = owner.record_authorization(request, &proposal).unwrap();
                (request, digest)
            }
            None => (
                NativeRequestId::new([seed; 16]).unwrap(),
                RequestDigest::from_bytes([seed; 32]),
            ),
        };
        let claim = VerifiedDeviceClaim::verify(proposal, &proof_device, &proof_auth).unwrap();
        let issued = self
            .service
            .issue_owner_authorization(owner_request, owner_digest, claim)
            .await
            .unwrap();
        let PairingOutcome::AuthorizationReserved { ticket, .. } = issued.outcome else {
            panic!("reservation");
        };
        let snapshot = self.server.handle().pairing_snapshot().unwrap();
        let reservation = snapshot.reservation(&ticket).unwrap();
        let lookup = PairingProtocolLookup::new(self.server.handle());
        let (client, hello) = EnrollmentClient::begin(
            self.identity.pin(),
            PublicEnrollmentReservation::from_wire(&reservation_to_wire(reservation)).unwrap(),
            vec![major],
            device.clone(),
            auth.clone(),
        )
        .unwrap();
        let (server, hello) = EnrollmentServer::accept(
            self.identity.pin().identity,
            vec![major],
            self.identity.clone(),
            &lookup,
            &hello,
        )
        .unwrap();
        let (client, proof) = client.receive_server(&hello).unwrap();
        let (mut server, ready) = server.receive_proof(&lookup, &proof).unwrap();
        let mut client = client.receive_ready(&ready).unwrap();
        let submit = client
            .submission_message(issued.authorization.as_ref().unwrap(), device.as_ref())
            .unwrap();
        let reply = self
            .service
            .enrollment_frame(&mut server, &submit)
            .await
            .unwrap();
        let spaceterm_protocol::wire::pairing::enrollment_message::Kind::Result(result) =
            client.decode(&reply).unwrap().kind.unwrap()
        else {
            panic!("enrolled");
        };
        let receipt = ReceiptCodec::new(self.identity.pin())
            .from_wire(&result.receipt.unwrap())
            .unwrap();
        let PairingReceiptBody::Enrollment { client, .. } = &receipt.parts().claims else {
            panic!("enrolled receipt");
        };
        let streams = client.clone();
        let principal = authentication::Principal {
            server: self.identity.pin().identity,
            device: [seed; 16],
            pairing: native::local(streams.parts().client.pairing.local()),
            client: native::local(streams.parts().client.id.local()),
        };
        (streams, principal, auth)
    }
}

async fn handshake(
    transport: &dyn MessageTransport,
    pin: authentication::ServerPin,
    principal: authentication::Principal,
    key: Arc<DeviceKey>,
    major: u32,
) -> spaceterm_protocol::connection::AuthenticatedConnection {
    let config = authentication::HandshakeConfig {
        majors: vec![major],
        capabilities: vec![
            spaceterm_protocol::negotiation::CAPABILITY_NORMALIZED_CREATION_TOPOLOGY,
            spaceterm_protocol::negotiation::CAPABILITY_SERVER_GIT,
            spaceterm_protocol::negotiation::CAPABILITY_SERVER_DIRECTORIES,
            spaceterm_protocol::negotiation::CAPABILITY_CANONICAL_DOMAIN,
            spaceterm_protocol::negotiation::CAPABILITY_ORGANIZATION_COMPARISONS,
            spaceterm_protocol::negotiation::CAPABILITY_TERMINAL_STREAMS,
            spaceterm_protocol::negotiation::CAPABILITY_NATIVE_NOTIFICATIONS,
            spaceterm_protocol::negotiation::CAPABILITY_ACCOUNT_RUNTIME,
        ],
        ..Default::default()
    };
    let (client, hello) = authentication::ClientHandshake::begin(
        pin,
        principal,
        authentication::Purpose::Ordinary,
        config,
        key,
    )
    .unwrap();
    transport.send(&hello).await.unwrap();
    let (client, proof) = client
        .receive_server(&transport.recv().await.unwrap().unwrap())
        .unwrap();
    transport.send(&proof).await.unwrap();
    let authentication::Established::Ordinary(codec) = client
        .receive_ready(&transport.recv().await.unwrap().unwrap())
        .unwrap()
    else {
        panic!("ordinary");
    };
    assert!(
        codec
            .capabilities()
            .contains(&spaceterm_protocol::negotiation::CAPABILITY_CANONICAL_DOMAIN)
    );
    assert!(
        codec
            .capabilities()
            .contains(&spaceterm_protocol::negotiation::CAPABILITY_NATIVE_NOTIFICATIONS)
    );
    codec
}
async fn round_trip(
    transport: &dyn MessageTransport,
    codec: &mut spaceterm_protocol::connection::AuthenticatedConnection,
    payload: Payload,
) -> Payload {
    transport
        .send(&codec.encode(&payload).unwrap())
        .await
        .unwrap();
    codec
        .decode(&transport.recv().await.unwrap().unwrap())
        .unwrap()
}
async fn reconcile(
    transport: &dyn MessageTransport,
    codec: &mut spaceterm_protocol::connection::AuthenticatedConnection,
    handle: &ServerHandle,
) -> u64 {
    let principal = codec.principal();
    let snapshot = handle.snapshot();
    macro_rules! profile {
        ($p:ident, $major:ident) => {
            Payload::$major($p::Message {
                kind: Some($p::message::Kind::Reconciliation($p::Reconciliation {
                    request: 7,
                    server: Some(spaceterm_protocol::convert::identity::encode_global(
                        snapshot.state().server.bytes(),
                    )),
                    client: Some(spaceterm_protocol::convert::identity::encode_client(
                        ClientId::new(
                            snapshot.state().server,
                            u64::from_be_bytes(principal.client[8..].try_into().unwrap()),
                        )
                        .unwrap(),
                    )),
                    epoch: snapshot.epoch().get(),
                    organization_revision: snapshot.state().revision.get(),
                    ..Default::default()
                })),
            })
        };
    }
    let request = if codec.major() == 1 {
        profile!(v1, Major1)
    } else {
        profile!(v2, Major2)
    };
    match round_trip(transport, codec, request).await {
        Payload::Major1(v1::Message {
            kind: Some(v1::message::Kind::Reconciliation(value)),
        }) => {
            assert!(value.acknowledged);
            value.connection_generation
        }
        Payload::Major2(v2::Message {
            kind: Some(v2::message::Kind::Reconciliation(value)),
        }) => {
            assert!(value.acknowledged);
            value.connection_generation
        }
        _ => panic!("reconciliation"),
    }
}
fn request_payload(request: &CanonicalOperationRequest, major: u32) -> Payload {
    if major == 1 {
        Payload::Major1(v1::Message {
            kind: Some(v1::message::Kind::Operation(
                operations::v1::encode_request(request).unwrap(),
            )),
        })
    } else {
        Payload::Major2(v2::Message {
            kind: Some(v2::message::Kind::Operation(
                operations::v2::encode_request(request).unwrap(),
            )),
        })
    }
}
fn decode_outcome(payload: Payload, id: OperationId) -> Outcome {
    match payload {
        Payload::Major1(v1::Message {
            kind: Some(v1::message::Kind::Outcome(value)),
        }) => operations::v1::decode_outcome(&value, id).unwrap().1,
        Payload::Major2(v2::Message {
            kind: Some(v2::message::Kind::Outcome(value)),
        }) => operations::v2::decode_outcome(&value, id).unwrap().1,
        _ => panic!("operation outcome"),
    }
}
async fn query_status(
    transport: &dyn MessageTransport,
    codec: &mut spaceterm_protocol::connection::AuthenticatedConnection,
) -> ServerStatus {
    let query = if codec.major() == 1 {
        Payload::Major1(v1::Message {
            kind: Some(v1::message::Kind::StatusQuery(v1::StatusQuery {
                request: 1,
                request_generation: 1,
                ..Default::default()
            })),
        })
    } else {
        Payload::Major2(v2::Message {
            kind: Some(v2::message::Kind::StatusQuery(v2::StatusQuery {
                request: 1,
                request_generation: 1,
                ..Default::default()
            })),
        })
    };
    transport
        .send(&codec.encode(&query).unwrap())
        .await
        .unwrap();
    let publication = loop {
        let payload = codec
            .decode(&transport.recv().await.unwrap().unwrap())
            .unwrap();
        let publication = match payload {
            Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::StatusPublication(value)),
            }) => management::v1::decode_publication(&value, &|_| {
                Err(ProtocolError::UnexpectedMessage)
            })
            .unwrap(),
            Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::StatusPublication(value)),
            }) => management::v2::decode_publication(&value, &|_| {
                Err(ProtocolError::UnexpectedMessage)
            })
            .unwrap(),
            Payload::Major1(v1::Message {
                kind:
                    Some(
                        v1::message::Kind::OrganizationSnapshot(_)
                        | v1::message::Kind::OrganizationDelta(_)
                        | v1::message::Kind::OrganizationResync(_),
                    ),
            })
            | Payload::Major2(v2::Message {
                kind:
                    Some(
                        v2::message::Kind::OrganizationSnapshot(_)
                        | v2::message::Kind::OrganizationDelta(_)
                        | v2::message::Kind::OrganizationResync(_),
                    ),
            }) => continue,
            _ => panic!("status publication"),
        };
        break publication;
    };
    let StatusPublication::Snapshot { status, .. } = publication else {
        panic!("snapshot");
    };
    status
}

async fn inventory_query(
    transport: &dyn MessageTransport,
    codec: &mut spaceterm_protocol::connection::AuthenticatedConnection,
    after: u64,
    version: u64,
) -> (u64, u64, Vec<u64>, i32) {
    macro_rules! query {
        ($p:ident, $variant:ident) => {
            Payload::$variant($p::Message {
                kind: Some($p::message::Kind::PairingRequest($p::PairingRequest {
                    request: 99,
                    action: Some($p::pairing_request::Action::List(true)),
                    inventory_after: after,
                    inventory_version: version,
                    request_generation: 1,
                    ..Default::default()
                })),
            })
        };
    }
    let request = if codec.major() == 1 {
        query!(v1, Major1)
    } else {
        query!(v2, Major2)
    };
    transport
        .send(&codec.encode(&request).unwrap())
        .await
        .unwrap();
    loop {
        let reply = codec
            .decode(&transport.recv().await.unwrap().unwrap())
            .unwrap();
        macro_rules! page {
            ($value:ident) => {{
                assert_eq!($value.request, 99);
                assert_eq!($value.request_generation, 1);
                assert_eq!(
                    spaceterm_protocol::convert::identity::decode_global(
                        $value.this_device.as_ref().unwrap()
                    )
                    .unwrap(),
                    codec.principal().device
                );
                return (
                    $value.inventory_version,
                    $value.inventory_total,
                    $value
                        .devices
                        .iter()
                        .map(|value| {
                            spaceterm_protocol::convert::identity::decode_local(
                                value.pairing.as_ref().unwrap(),
                            )
                            .unwrap()
                        })
                        .collect(),
                    $value.classified_failure,
                );
            }};
        }
        match reply {
            Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::PairingOutcome(value)),
            }) => page!(value),
            Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::PairingOutcome(value)),
            }) => page!(value),
            _ => {}
        }
    }
}

#[tokio::test]
async fn servers_settings_paired_device_pages_remain_revision_coherent() {
    tokio::time::timeout(Duration::from_secs(30), async {
        for major in [1, 2] {
            let fixture = Fixture::new().await;
            let (first, principal, device) = fixture.enroll(major).await;
            let (second, _, _) = fixture.enroll_with_seed(major, 81).await;
            let handle = fixture.server.handle();
            let endpoint = fixture.registration.endpoint().unwrap();
            let factories = production_listener_factories(
                handle.clone(),
                ProductionListenerConfig {
                    registration: &fixture.registration,
                    identity: fixture.identity.clone(),
                    running: spaceterm_distribution::BuildVersion::parse("1.2.3").unwrap(),
                    codec: Arc::new(CanonicalCodec),
                    pairing: fixture.service.clone(),
                    ports: fixture.ports.clone(),
                },
                Arc::new(PairingProtocolLookup::new(handle.clone())),
            )
            .unwrap();
            let stop = fixture.stop.clone();
            let run = tokio::spawn(fixture.server.run(factories));
            while !endpoint.path().exists() {
                tokio::task::yield_now().await;
            }
            let ipc = IpcTransport::connect(&endpoint, MessageLimit::envelope())
                .await
                .unwrap();
            let mut codec = handshake(&ipc, fixture.identity.pin(), principal, device, major).await;
            reconcile(&ipc, &mut codec, &handle).await;
            let (version, total, devices, failure) = inventory_query(&ipc, &mut codec, 0, 0).await;
            assert_eq!((total, failure), (2, 0));
            assert_eq!(
                devices,
                vec![
                    first.parts().client.pairing.local(),
                    second.parts().client.pairing.local()
                ]
            );
            let (same, total, devices, failure) =
                inventory_query(&ipc, &mut codec, devices[0], version).await;
            assert_eq!((same, total, failure), (version, 2, 0));
            assert_eq!(devices, vec![second.parts().client.pairing.local()]);
            let changed = CanonicalOperationRequest::new(
                OperationId::new(first.parts().ordinary, OperationSequence::new(1).unwrap()),
                Command::CreateWorkspace {
                    server: ServerId::new(fixture.identity.pin().identity),
                    source: WorkspaceCreationSource::ServerHome {
                        server: ServerId::new(fixture.identity.pin().identity),
                    },
                    name: NameDraft::Automatic,
                },
                &CanonicalCodec,
            )
            .unwrap();
            let _ = decode_outcome(
                round_trip(&ipc, &mut codec, request_payload(&changed, major)).await,
                changed.binding().id,
            );
            let (_, _, devices, failure) = inventory_query(
                &ipc,
                &mut codec,
                first.parts().client.pairing.local(),
                version,
            )
            .await;
            assert!(devices.is_empty());
            assert_eq!(
                failure,
                spaceterm_protocol::wire::pairing::PairingFailure::PairingStaleAssessment as i32
            );
            ipc.close().await.unwrap();
            handle
                .shutdown(TrustedShutdownRequest { authority: stop })
                .await
                .unwrap();
            run.await.unwrap().unwrap();
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn production_ipc_and_wss_use_real_identity_pairing_and_reliable_actor_outcomes() {
    tokio::time::timeout(Duration::from_secs(30), async {
        for major in [1, 2] {
            let fixture = Fixture::new().await;
            let (streams, principal, device) = fixture.enroll(major).await;
            let (remote_streams, remote_principal, remote_device) =
                fixture.enroll_with_seed(major, 81).await;
            let (_, ipv6_principal, ipv6_device) = fixture.enroll_with_seed(major, 91).await;
            let handle = fixture.server.handle();
            let pin = fixture.identity.pin();
            let endpoint = fixture.registration.endpoint().unwrap();
            let factories = production_listener_factories(
                handle.clone(),
                ProductionListenerConfig {
                    registration: &fixture.registration,
                    identity: fixture.identity.clone(),
                    running: spaceterm_distribution::BuildVersion::parse("1.2.3").unwrap(),
                    codec: Arc::new(CanonicalCodec),
                    pairing: fixture.service.clone(),
                    ports: fixture.ports.clone(),
                },
                Arc::new(PairingProtocolLookup::new(handle.clone())),
            )
            .unwrap();
            let feed = fixture.ports.subscribe_status().unwrap();
            let run = tokio::spawn(fixture.server.run(factories));
            while !endpoint.path().exists() {
                tokio::task::yield_now().await;
            }
            let ipc = IpcTransport::connect(&endpoint, MessageLimit::envelope())
                .await
                .unwrap();
            assert_eq!(
                ipc.peer_facts().local_uid,
                Some(fixture.registration.key().account_uid)
            );
            let mut ipc_codec = handshake(&ipc, pin, principal, device.clone(), major).await;
            reconcile(&ipc, &mut ipc_codec, &handle).await;
            let initial = query_status(&ipc, &mut ipc_codec).await;
            assert_eq!(
                initial.parts().runtime,
                ServerRuntimeStatus {
                    storage: StorageStatus::Ready,
                    intent: ServerRunIntent::Running,
                }
            );
            assert!(!initial.parts().remote_access.value.is_enabled());
            assert!(initial.parts().listeners.is_empty());
            assert_eq!(initial.parts().epoch, handle.snapshot().epoch());
            assert_eq!(initial.parts().running.as_ref().unwrap().as_str(), "1.2.3");
            assert_eq!(
                initial.parts().native_verification,
                NativeVerification::Unverified
            );
            let reservation =
                tokio::net::TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
                    .await
                    .unwrap();
            let address = reservation.local_addr().unwrap();
            drop(reservation);
            let mut addresses = vec![address];
            match tokio::net::TcpListener::bind("[::1]:0").await {
                Ok(reservation) => addresses.push(reservation.local_addr().unwrap()),
                Err(error)
                    if matches!(
                        error.raw_os_error(),
                        Some(libc::EAFNOSUPPORT | libc::EADDRNOTAVAIL)
                    ) => {}
                Err(error) => panic!("IPv6 loopback probe failed: {error}"),
            }
            let version = handle
                .snapshot()
                .state()
                .retained
                .as_ref()
                .unwrap()
                .remote_access()
                .version;
            let enable = CanonicalOperationRequest::new(
                OperationId::new(streams.parts().ordinary, OperationSequence::new(1).unwrap()),
                Command::SetRemoteAccess {
                    server: ServerId::new(pin.identity),
                    expected_version: version,
                    value: RemoteAccessPolicy::enabled(
                        ServerId::new(pin.identity),
                        addresses.clone(),
                    )
                    .unwrap(),
                },
                &CanonicalCodec,
            )
            .unwrap();
            let _ = decode_outcome(
                round_trip(&ipc, &mut ipc_codec, request_payload(&enable, major)).await,
                enable.binding().id,
            );
            let enabled = query_status(&ipc, &mut ipc_codec).await;
            assert!(enabled.parts().remote_access.value.is_enabled());
            assert_eq!(enabled.parts().listeners.len(), addresses.len());
            for (fact, address) in enabled.parts().listeners.iter().zip(&addresses) {
                assert_eq!(fact.endpoint, *address);
                assert_eq!(fact.state, ListenerState::Listening);
                assert_eq!(fact.generation.get(), 2);
            }
            let wrong = WssTransport::connect(
                address,
                "localhost",
                EnrolledServerKey([9; 32]),
                MessageLimit::envelope(),
                Duration::from_secs(2),
            )
            .await;
            assert_eq!(wrong.err(), Some(TransportError::TlsIdentityRejected));
            let wss = WssTransport::connect(
                address,
                "localhost",
                EnrolledServerKey(pin.public_key),
                MessageLimit::envelope(),
                Duration::from_secs(2),
            )
            .await
            .unwrap();
            let mut codec = handshake(&wss, pin, remote_principal, remote_device, major).await;
            reconcile(&wss, &mut codec, &handle).await;
            let create = CanonicalOperationRequest::new(
                OperationId::new(
                    remote_streams.parts().ordinary,
                    OperationSequence::new(1).unwrap(),
                ),
                Command::CreateWorkspace {
                    server: ServerId::new(pin.identity),
                    source: WorkspaceCreationSource::ServerHome {
                        server: ServerId::new(pin.identity),
                    },
                    name: NameDraft::Automatic,
                },
                &CanonicalCodec,
            )
            .unwrap();
            let first = decode_outcome(
                round_trip(&wss, &mut codec, request_payload(&create, major)).await,
                create.binding().id,
            );
            assert_eq!(handle.snapshot().state().workspaces.len(), 1);
            let replay = decode_outcome(
                round_trip(&wss, &mut codec, request_payload(&create, major)).await,
                create.binding().id,
            );
            assert_eq!(first, replay);
            assert_eq!(handle.snapshot().state().workspaces.len(), 1);
            // Both IP families carry the same pinned authenticated protocol.
            let ipv6_transport = if let Some(ipv6) = addresses.get(1) {
                let transport = WssTransport::connect(
                    *ipv6,
                    "localhost",
                    EnrolledServerKey(pin.public_key),
                    MessageLimit::envelope(),
                    Duration::from_secs(2),
                )
                .await
                .unwrap();
                let mut ipv6_codec =
                    handshake(&transport, pin, ipv6_principal, ipv6_device, major).await;
                assert_eq!(
                    query_status(&transport, &mut ipv6_codec)
                        .await
                        .parts()
                        .server,
                    ServerId::new(pin.identity)
                );
                Some(transport)
            } else {
                None
            };
            let disable = CanonicalOperationRequest::new(
                OperationId::new(streams.parts().ordinary, OperationSequence::new(2).unwrap()),
                Command::SetRemoteAccess {
                    server: ServerId::new(pin.identity),
                    expected_version: enabled.parts().remote_access.version,
                    value: RemoteAccessPolicy::disabled(ServerId::new(pin.identity)),
                },
                &CanonicalCodec,
            )
            .unwrap();
            let _ = decode_outcome(
                round_trip(&ipc, &mut ipc_codec, request_payload(&disable, major)).await,
                disable.binding().id,
            );
            assert_eq!(wss.recv().await.unwrap(), None);
            if let Some(ipv6) = ipv6_transport {
                assert_eq!(ipv6.recv().await.unwrap(), None);
            }
            for address in &addresses {
                assert!(tokio::net::TcpStream::connect(address).await.is_err());
            }
            let disabled = query_status(&ipc, &mut ipc_codec).await;
            assert!(!disabled.parts().remote_access.value.is_enabled());
            assert!(disabled.parts().listeners.is_empty());
            assert_eq!(disabled.parts().epoch, initial.parts().epoch);
            ipc.close().await.unwrap();
            handle
                .shutdown(TrustedShutdownRequest {
                    authority: fixture.stop,
                })
                .await
                .unwrap();
            assert!(run.await.unwrap().unwrap().listeners_closed);
            assert_eq!(handle.runtime_status().intent, ServerRunIntent::Stopped);
            assert_eq!(
                feed.borrow().parts().runtime.intent,
                ServerRunIntent::Stopped
            );
            assert!(feed.borrow().parts().listeners.is_empty());
            assert!(!endpoint.path().exists());
            assert!(tokio::net::TcpStream::connect(address).await.is_err());
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn identified_actor_admission_requires_a_live_connection_reply_reservation() {
    let mut fixture = native::Fixture::new();
    let outcomes = ProductionListenerPorts::new();
    fixture.config.outcomes = outcomes.outcomes.clone();
    let principal = fixture.principal(1, 1);
    let database = fixture.db.clone();
    let codec = fixture.codec.clone();
    let stop = fixture.host.clone();
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    native::connect(&handle, principal).await;
    let commits = database.lock().unwrap().transactions.len();
    let request = native::request(
        &codec,
        1,
        1,
        Command::RenameWorkspace {
            workspace: native::workspace(),
            expected_version: spaceterm_core::authority::FieldVersion::new(1),
            draft: NameDraft::Custom(NameText::new("must not commit".into()).unwrap()),
        },
    );
    assert_eq!(
        handle.submit(principal, request).await.err().unwrap().class,
        ServerFailureClass::Capacity
    );
    assert_eq!(database.lock().unwrap().transactions.len(), commits);
    handle
        .shutdown(TrustedShutdownRequest { authority: stop })
        .await
        .unwrap();
    server.wait().await.unwrap();
}

#[tokio::test]
async fn listener_owner_shutdown_joins_actor_and_removes_endpoint() {
    let fixture = Fixture::new().await;
    let handle = fixture.server.handle();
    let endpoint = fixture.registration.endpoint().unwrap();
    let listeners = production_listener_factories(
        handle.clone(),
        ProductionListenerConfig {
            registration: &fixture.registration,
            identity: fixture.identity.clone(),
            running: spaceterm_distribution::BuildVersion::parse("1.0.0").unwrap(),
            codec: Arc::new(CanonicalCodec),
            pairing: fixture.service,
            ports: fixture.ports,
        },
        Arc::new(PairingProtocolLookup::new(handle.clone())),
    )
    .unwrap();
    let (shutdown, received) = tokio::sync::oneshot::channel();
    let run = tokio::spawn(fixture.server.run_until(listeners, async {
        let _ = received.await;
    }));
    tokio::time::timeout(Duration::from_secs(5), async {
        while handle.phase() != ServicePhase::Serving {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(endpoint.path().exists());
    shutdown.send(()).unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .unwrap()
        .unwrap();
    let report = result.unwrap();
    assert!(report.cleanup_confirmed && report.listeners_closed && report.started_writes_finished);
    assert!(!report.recovery_required);
    assert!(!endpoint.path().exists());
    assert_eq!(handle.phase(), ServicePhase::Stopped);
    assert_eq!(handle.runtime_status().intent, ServerRunIntent::Running);
    assert_eq!(handle.runtime_status().storage, StorageStatus::Ready);
}

#[cfg(feature = "test-support")]
#[path = "support/account_composition.rs"]
mod account_composition;

#[tokio::test]
async fn actor_status_distinguishes_locked_unavailable_and_a_successful_storage_retry() {
    use spaceterm_core::storage::{CommitDisposition, StoreFailure};
    for (error, storage) in [
        (StoreFailure::Locked, StorageStatus::Locked),
        (StoreFailure::Unavailable, StorageStatus::Unavailable),
        (StoreFailure::Io, StorageStatus::Unavailable),
    ] {
        let fixture = native::Fixture::new();
        let principal = fixture.principal(1, 1);
        let db = fixture.db.clone();
        let codec = fixture.codec.clone();
        let stop = fixture.host.clone();
        let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
            .await
            .unwrap();
        let handle = server.handle();
        native::connect(&handle, principal).await;
        let request = native::request(
            &codec,
            1,
            1,
            Command::RenameWorkspace {
                workspace: native::workspace(),
                expected_version: spaceterm_core::authority::FieldVersion::new(1),
                draft: NameDraft::Custom(NameText::new("storage retry".into()).unwrap()),
            },
        );
        db.lock().unwrap().next = Some(CommitDisposition::NotApplied(error));
        assert_eq!(
            handle
                .submit(principal, request.clone())
                .await
                .err()
                .unwrap()
                .class,
            ServerFailureClass::StorageUnavailable
        );
        assert_eq!(handle.runtime_status().storage, storage);
        assert_eq!(handle.runtime_status().intent, ServerRunIntent::Running);
        handle.submit(principal, request).await.unwrap();
        assert_eq!(handle.runtime_status().storage, StorageStatus::Ready);
        handle
            .shutdown(TrustedShutdownRequest { authority: stop })
            .await
            .unwrap();
        server.wait().await.unwrap();
        assert_eq!(handle.runtime_status().intent, ServerRunIntent::Stopped);
    }
}

#[tokio::test]
async fn indeterminate_storage_status_requires_recovery_without_claiming_deliberate_stop() {
    use spaceterm_core::storage::CommitDisposition;
    let fixture = native::Fixture::new();
    let principal = fixture.principal(1, 1);
    let db = fixture.db.clone();
    let codec = fixture.codec.clone();
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    native::connect(&handle, principal).await;
    db.lock().unwrap().next = Some(CommitDisposition::Indeterminate);
    let request = native::request(
        &codec,
        1,
        1,
        Command::RenameWorkspace {
            workspace: native::workspace(),
            expected_version: spaceterm_core::authority::FieldVersion::new(1),
            draft: NameDraft::Custom(NameText::new("uncertain write".into()).unwrap()),
        },
    );
    assert_eq!(
        handle.submit(principal, request).await.err().unwrap().class,
        ServerFailureClass::RecoveryRequired
    );
    assert_eq!(
        server.wait().await.unwrap_err().class,
        ServerFailureClass::RecoveryRequired
    );
    assert_eq!(
        handle.runtime_status(),
        ServerRuntimeStatus {
            storage: StorageStatus::RecoveryRequired,
            intent: ServerRunIntent::Running,
        }
    );
}

#[tokio::test]
async fn forwarded_private_ipc_authenticates_and_reconnects_without_network_enablement() {
    tokio::time::timeout(Duration::from_secs(30), async {
        for major in [1, 2] {
            let fixture = Fixture::new_registered(true).await;
            let (_, principal, device) = fixture.enroll(major).await;
            let handle = fixture.server.handle();
            let endpoint = fixture.registration.endpoint().unwrap();
            let listeners = production_listener_factories(
                handle.clone(),
                ProductionListenerConfig {
                    registration: &fixture.registration,
                    identity: fixture.identity.clone(),
                    running: spaceterm_distribution::BuildVersion::parse("1.2.3").unwrap(),
                    codec: Arc::new(CanonicalCodec),
                    pairing: fixture.service,
                    ports: fixture.ports,
                },
                Arc::new(PairingProtocolLookup::new(handle.clone())),
            )
            .unwrap();
            let run = tokio::spawn(fixture.server.run(listeners));
            while !endpoint.path().exists() {
                tokio::task::yield_now().await;
            }
            let retained = handle
                .snapshot()
                .state()
                .retained
                .as_ref()
                .unwrap()
                .remote_access()
                .clone();
            assert!(!retained.value.is_enabled());
            let private = fixture._root.path().join("forwarding");
            fs::create_dir(&private).unwrap();
            fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).unwrap();
            let directory = spaceterm_host::ipc::PrivateIpcDirectory::open(&private).unwrap();
            for attempt in 0..2 {
                // This injected stream-local proxy carries the same bytes as
                // OpenSSH -L. It adds no pairing or remote-listener authority.
                let forwarding = directory.endpoint(&format!("forward-{attempt}")).unwrap();
                let proxy = tokio::net::UnixListener::bind(forwarding.path()).unwrap();
                fs::set_permissions(forwarding.path(), fs::Permissions::from_mode(0o600)).unwrap();
                let remote = endpoint.clone();
                let proxy = tokio::spawn(async move {
                    let (mut client, _) = proxy.accept().await.unwrap();
                    let mut server = tokio::net::UnixStream::connect(remote.path())
                        .await
                        .unwrap();
                    if let Err(error) =
                        tokio::io::copy_bidirectional(&mut client, &mut server).await
                    {
                        // Closing authenticated IPC may close both halves before
                        // the proxy's final shutdown syscall, especially on Darwin.
                        assert!(matches!(
                            error.raw_os_error(),
                            Some(libc::ENOTCONN | libc::ECONNRESET | libc::EPIPE)
                        ));
                    }
                });
                let transport = IpcTransport::connect(&forwarding, MessageLimit::envelope())
                    .await
                    .unwrap();
                let mut codec = handshake(
                    &transport,
                    fixture.identity.pin(),
                    principal,
                    device.clone(),
                    major,
                )
                .await;
                let status = query_status(&transport, &mut codec).await;
                assert_eq!(status.parts().remote_access, retained);
                assert!(status.parts().listeners.is_empty());
                // Native discovery neither opens a listener nor changes policy.
                assert_eq!(
                    fixture
                        .owner
                        .as_ref()
                        .unwrap()
                        .discover()
                        .unwrap()
                        .unwrap()
                        .server_pin(),
                    fixture.identity.pin()
                );
                assert_eq!(
                    handle
                        .snapshot()
                        .state()
                        .retained
                        .as_ref()
                        .unwrap()
                        .remote_access(),
                    &retained
                );
                transport.close().await.unwrap();
                proxy.await.unwrap();
                fs::remove_file(forwarding.path()).unwrap();
            }
            handle
                .shutdown(TrustedShutdownRequest {
                    authority: fixture.stop,
                })
                .await
                .unwrap();
            assert!(run.await.unwrap().unwrap().listeners_closed);
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn native_status_observations_reach_both_majors_and_clear_after_provider_failure() {
    use spaceterm_server::listeners::{NativeStatusObservation, NativeStatusObserver};
    use std::sync::atomic::{AtomicBool, Ordering};
    struct Native(AtomicBool);
    impl NativeStatusObserver for Native {
        fn observe(
            &self,
            _: ServerId,
        ) -> ServerFuture<'_, Result<NativeStatusObservation, ServerFailure>> {
            Box::pin(async {
                if self.0.load(Ordering::SeqCst) {
                    return Err(spaceterm_server::failure::failure(
                        ServerFailureClass::NativeUnavailable,
                    ));
                }
                Ok(NativeStatusObservation {
                    approved: Some(ServerVersion::new("1.2.3".into()).unwrap()),
                    staged: Some(ServerVersion::new("1.2.4".into()).unwrap()),
                    verification: NativeVerification::Verified,
                    capabilities: vec![CapabilityStatus {
                        capability: CAPABILITY_NATIVE_LIFECYCLE,
                        availability: CapabilityAvailability::AuthorizationRequired,
                    }],
                    progress: vec![ManagementProgress {
                        request: ManagementRequestReference::Native([7; 16]),
                        state: ManagementProgressState::Draining,
                        completed_units: 1,
                        total_units: 2,
                    }],
                })
            })
        }
    }
    for major in [1, 2] {
        let native = Arc::new(Native(AtomicBool::new(false)));
        let fixture = Fixture::with_native_status(false, Some(native.clone())).await;
        let (_, principal, device) = fixture.enroll(major).await;
        let handle = fixture.server.handle();
        let endpoint = fixture.registration.endpoint().unwrap();
        let factories = production_listener_factories(
            handle.clone(),
            ProductionListenerConfig {
                registration: &fixture.registration,
                identity: fixture.identity.clone(),
                running: spaceterm_distribution::BuildVersion::parse("1.2.3").unwrap(),
                codec: Arc::new(CanonicalCodec),
                pairing: fixture.service.clone(),
                ports: fixture.ports.clone(),
            },
            Arc::new(PairingProtocolLookup::new(handle.clone())),
        )
        .unwrap();
        let mut status = fixture.ports.subscribe_status().unwrap();
        let running = tokio::spawn(fixture.server.run(factories));
        tokio::time::timeout(Duration::from_secs(5), async {
            while status.borrow().parts().native_verification != NativeVerification::Verified {
                status.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        let ipc = IpcTransport::connect(&endpoint, MessageLimit::envelope())
            .await
            .unwrap();
        let mut codec = handshake(&ipc, fixture.identity.pin(), principal, device, major).await;
        let first = query_status(&ipc, &mut codec).await;
        assert_eq!(first.parts().approved.as_ref().unwrap().as_str(), "1.2.3");
        assert_eq!(first.parts().staged.as_ref().unwrap().as_str(), "1.2.4");
        assert_eq!(
            first.parts().capabilities[0].availability,
            CapabilityAvailability::AuthorizationRequired
        );
        assert_eq!(
            first.parts().progress[0].state,
            ManagementProgressState::Draining
        );
        native.0.store(true, Ordering::SeqCst);
        tokio::time::timeout(Duration::from_secs(5), async {
            while status.borrow().parts().native_verification == NativeVerification::Verified {
                status.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        let failed = query_status(&ipc, &mut codec).await;
        assert_eq!(
            failed.parts().native_verification,
            NativeVerification::Unverified
        );
        assert!(failed.parts().approved.is_none());
        assert!(failed.parts().staged.is_none());
        assert_eq!(failed.parts().capabilities.len(), 2);
        assert!(
            failed
                .parts()
                .capabilities
                .iter()
                .all(|value| value.availability == CapabilityAvailability::Unavailable)
        );
        assert!(failed.parts().revision > first.parts().revision);
        assert!(!failed.parts().remote_access.value.is_enabled());
        ipc.close().await.unwrap();
        handle
            .shutdown(TrustedShutdownRequest {
                authority: fixture.stop,
            })
            .await
            .unwrap();
        running.await.unwrap().unwrap();
    }
}

#[tokio::test]
async fn production_focus_and_acknowledgement_survive_another_clients_pending_commit_in_both_majors()
 {
    tokio::time::timeout(Duration::from_secs(30), async {
        for major in [1, 2] {
            let fixture = Fixture::new().await;
            let (first_streams, first_principal, first_key) = fixture.enroll(major).await;
            let (second_streams, second_principal, second_key) =
                fixture.enroll_with_seed(major, 81).await;
            let handle = fixture.server.handle();
            let endpoint = fixture.registration.endpoint().unwrap();
            let listeners = production_listener_factories(
                handle.clone(),
                ProductionListenerConfig {
                    registration: &fixture.registration,
                    identity: fixture.identity.clone(),
                    running: spaceterm_distribution::BuildVersion::parse("1.2.3").unwrap(),
                    codec: Arc::new(CanonicalCodec),
                    pairing: fixture.service.clone(),
                    ports: fixture.ports.clone(),
                },
                Arc::new(PairingProtocolLookup::new(handle.clone())),
            )
            .unwrap();
            let run = tokio::spawn(fixture.server.run(listeners));
            while handle.phase() != ServicePhase::Serving {
                tokio::task::yield_now().await;
            }
            let first = IpcTransport::connect(&endpoint, MessageLimit::envelope())
                .await
                .unwrap();
            let second = IpcTransport::connect(&endpoint, MessageLimit::envelope())
                .await
                .unwrap();
            let mut first_codec = handshake(
                &first,
                fixture.identity.pin(),
                first_principal,
                first_key,
                major,
            )
            .await;
            let mut second_codec = handshake(
                &second,
                fixture.identity.pin(),
                second_principal,
                second_key,
                major,
            )
            .await;
            reconcile(&first, &mut first_codec, &handle).await;
            let second_connection = reconcile(&second, &mut second_codec, &handle).await;
            let server_id = ServerId::new(fixture.identity.pin().identity);
            for (transport, codec, stream) in [
                (&first, &mut first_codec, first_streams.parts().ordinary),
                (&second, &mut second_codec, second_streams.parts().ordinary),
            ] {
                let create = CanonicalOperationRequest::new(
                    OperationId::new(stream, OperationSequence::new(1).unwrap()),
                    Command::CreateWorkspace {
                        server: server_id,
                        source: WorkspaceCreationSource::ServerHome { server: server_id },
                        name: NameDraft::Automatic,
                    },
                    &CanonicalCodec,
                )
                .unwrap();
                assert!(matches!(
                    decode_outcome(
                        round_trip(transport, codec, request_payload(&create, major)).await,
                        create.binding().id
                    ),
                    Outcome::CreatedWithControl(_)
                ));
            }
            let snapshot = handle.snapshot();
            let workspace = &snapshot.state().workspaces[0];
            let gate = native::StoreGate::new();
            *fixture.write_gate.lock().unwrap() = Some(gate.clone());
            let rename = CanonicalOperationRequest::new(
                OperationId::new(
                    first_streams.parts().ordinary,
                    OperationSequence::new(2).unwrap(),
                ),
                Command::RenameWorkspace {
                    workspace: workspace.id,
                    expected_version: workspace.name.version,
                    draft: NameDraft::Custom(NameText::new("shared commit".into()).unwrap()),
                },
                &CanonicalCodec,
            )
            .unwrap();
            first
                .send(
                    &first_codec
                        .encode(&request_payload(&rename, major))
                        .unwrap(),
                )
                .await
                .unwrap();
            gate.entered().await;
            macro_rules! frames {
                ($p:ident, $variant:ident) => {
                    (
                        Payload::$variant($p::Message {
                            kind: Some($p::message::Kind::ClientContext($p::ClientContext {
                                request: 40,
                                epoch: snapshot.epoch().get(),
                                connection_generation: second_connection,
                                context_generation: 2,
                                focused_pane: None,
                                terminal_input_focus: false,
                                native_effects_eligible: false,
                                acknowledged: false,
                                failure: None,
                            })),
                        }),
                        Payload::$variant($p::Message {
                            kind: Some($p::message::Kind::Acknowledgement(
                                operations::$p::encode_acknowledgement(
                                    second_streams.parts().ordinary,
                                    OperationSequence::new(1).unwrap(),
                                ),
                            )),
                        }),
                    )
                };
            }
            let (focus, acknowledgement) = if major == 1 {
                frames!(v1, Major1)
            } else {
                frames!(v2, Major2)
            };
            second
                .send(&second_codec.encode(&focus).unwrap())
                .await
                .unwrap();
            second
                .send(&second_codec.encode(&acknowledgement).unwrap())
                .await
                .unwrap();
            let early = tokio::time::timeout(Duration::from_millis(50), second.recv()).await;
            gate.release();
            assert!(
                early.is_err(),
                "context waits instead of disconnecting during serialization: {early:?}"
            );
            let mut focused = false;
            let mut acknowledged = false;
            while !focused || !acknowledged {
                let payload = second_codec
                    .decode(
                        &second
                            .recv()
                            .await
                            .unwrap()
                            .expect("authenticated peer remains open"),
                    )
                    .unwrap();
                match payload {
                    Payload::Major1(v1::Message {
                        kind: Some(v1::message::Kind::ClientContext(value)),
                    }) => {
                        assert!(value.acknowledged);
                        assert!(value.failure.is_none());
                        focused = true;
                    }
                    Payload::Major2(v2::Message {
                        kind: Some(v2::message::Kind::ClientContext(value)),
                    }) => {
                        assert!(value.acknowledged);
                        assert!(value.failure.is_none());
                        focused = true;
                    }
                    Payload::Major1(v1::Message {
                        kind: Some(v1::message::Kind::AcknowledgementConfirmation(value)),
                    }) => {
                        assert_eq!(value.acknowledged_through, 1);
                        acknowledged = true;
                    }
                    Payload::Major2(v2::Message {
                        kind: Some(v2::message::Kind::AcknowledgementConfirmation(value)),
                    }) => {
                        assert_eq!(value.acknowledged_through, 1);
                        acknowledged = true;
                    }
                    _ => panic!("context and acknowledgement replies"),
                }
            }
            assert_eq!(
                query_status(&second, &mut second_codec)
                    .await
                    .parts()
                    .server,
                server_id
            );
            let payload = first_codec
                .decode(&first.recv().await.unwrap().unwrap())
                .unwrap();
            decode_outcome(payload, rename.binding().id);
            macro_rules! stale_reconciliation {
                ($p:ident, $variant:ident) => {
                    Payload::$variant($p::Message {
                        kind: Some($p::message::Kind::Reconciliation($p::Reconciliation {
                            request: 41,
                            request_generation: 3,
                            server: Some(spaceterm_protocol::convert::identity::encode_global(
                                server_id.bytes(),
                            )),
                            client: Some(spaceterm_protocol::convert::identity::encode_client(
                                second_streams.parts().client.id,
                            )),
                            epoch: snapshot.epoch().get(),
                            organization_revision: snapshot.state().revision.get(),
                            ..Default::default()
                        })),
                    })
                };
            }
            let stale = if major == 1 {
                stale_reconciliation!(v1, Major1)
            } else {
                stale_reconciliation!(v2, Major2)
            };
            second
                .send(&second_codec.encode(&stale).unwrap())
                .await
                .unwrap();
            let response = second_codec
                .decode(
                    &second
                        .recv()
                        .await
                        .unwrap()
                        .expect("stale organization comparison must not disconnect the peer"),
                )
                .unwrap();
            match response {
                Payload::Major1(v1::Message {
                    kind: Some(v1::message::Kind::OrganizationResync(value)),
                }) => {
                    assert_eq!(
                        (value.request, value.request_generation, value.revision),
                        (41, 3, handle.snapshot().state().revision.get())
                    );
                }
                Payload::Major2(v2::Message {
                    kind: Some(v2::message::Kind::OrganizationResync(value)),
                }) => {
                    assert_eq!(
                        (value.request, value.request_generation, value.revision),
                        (41, 3, handle.snapshot().state().revision.get())
                    );
                }
                _ => panic!("stale reconciliation requires an authoritative organization resync"),
            }
            reconcile(&second, &mut second_codec, &handle).await;
            assert_eq!(
                query_status(&second, &mut second_codec)
                    .await
                    .parts()
                    .server,
                server_id
            );
            first.close().await.unwrap();
            second.close().await.unwrap();
            handle
                .shutdown(TrustedShutdownRequest {
                    authority: fixture.stop,
                })
                .await
                .unwrap();
            run.await.unwrap().unwrap();
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn production_listener_shutdown_joins_a_cancelled_directory_observation() {
    use spaceterm_core::server_directories::*;
    use spaceterm_server::{
        git::directories::{DirectoryOrdinaryDispatch, DirectoryService},
        listeners::production_listener_factories_with_dispatch,
        protocol_dispatch::ActorOrdinaryDispatch,
    };
    struct PausedDirectory {
        provider: spaceterm_host::server_directories::native::NativeDirectoryProvider,
        gate: Arc<native::StoreGate>,
    }
    impl ServerDirectoryQueries for PausedDirectory {
        fn query(
            &self,
            request: &DirectoryQuery,
        ) -> Result<DirectoryQueryResult, DirectoryFailure> {
            self.gate.block();
            self.provider.query(request)
        }
        fn containment(
            &self,
            ancestor: &ServerDirectory,
            candidate: &ServerDirectory,
        ) -> Result<DirectoryContainment, DirectoryFailure> {
            self.provider.containment(ancestor, candidate)
        }
    }
    tokio::time::timeout(Duration::from_secs(15), async {
        let fixture = Fixture::new().await;
        let (_, principal, key) = fixture.enroll(2).await;
        let handle = fixture.server.handle();
        let server_id = ServerId::new(fixture.identity.pin().identity);
        let gate = native::StoreGate::new();
        let directories = Arc::new(DirectoryService::new(
            server_id,
            Arc::new(PausedDirectory {
                provider: spaceterm_host::server_directories::native::NativeDirectoryProvider::new(
                    server_id,
                    fixture._root.path().to_owned(),
                )
                .unwrap(),
                gate: gate.clone(),
            }),
            Arc::new(handle.clone()),
        ));
        let listeners = production_listener_factories_with_dispatch(
            handle.clone(),
            ProductionListenerConfig {
                registration: &fixture.registration,
                identity: fixture.identity.clone(),
                running: spaceterm_distribution::BuildVersion::parse("1.2.3").unwrap(),
                codec: Arc::new(CanonicalCodec),
                pairing: fixture.service.clone(),
                ports: fixture.ports.clone(),
            },
            Arc::new(PairingProtocolLookup::new(handle.clone())),
            Arc::new(DirectoryOrdinaryDispatch::new(
                directories.clone(),
                Arc::new(ActorOrdinaryDispatch::new(Arc::new(CanonicalCodec))),
            )),
        )
        .unwrap();
        let lifecycle = listeners.lifecycle.clone();
        let run = tokio::spawn(fixture.server.run(listeners));
        while handle.phase() != ServicePhase::Serving {
            tokio::task::yield_now().await;
        }
        let transport = IpcTransport::connect(
            &fixture.registration.endpoint().unwrap(),
            MessageLimit::envelope(),
        )
        .await
        .unwrap();
        let mut codec = handshake(&transport, fixture.identity.pin(), principal, key, 2).await;
        let generation = spaceterm_core::authority::ConnectionGeneration::new(
            reconcile(&transport, &mut codec, &handle).await,
        );
        let principal = VerifiedPrincipal::from_authenticated(&codec, generation).unwrap();
        let context = DirectoryContext::new(DirectoryContextParts {
            server: principal.server(),
            epoch: principal.epoch(),
            pairing: principal.pairing(),
            client: principal.client(),
        })
        .unwrap();
        let request = spaceterm_protocol::convert::directories::DirectoryRequest::new(
            1,
            1,
            DirectoryQuery::new(DirectoryQueryParts {
                context,
                action: DirectoryQueryAction::Home,
            })
            .unwrap(),
        )
        .unwrap();
        let payload = spaceterm_protocol::convert::directories::encode_query(2, &request).unwrap();
        transport
            .send(&codec.encode(&payload).unwrap())
            .await
            .unwrap();
        gate.entered().await;
        let mut closing = tokio::spawn(async move { lifecycle.close().await });
        let early = tokio::time::timeout(Duration::from_millis(50), &mut closing).await;
        gate.release();
        assert!(
            early.is_err(),
            "listener shutdown must join cancelled filesystem work: {early:?}"
        );
        closing.await.unwrap().unwrap();
        handle
            .shutdown(TrustedShutdownRequest {
                authority: fixture.stop,
            })
            .await
            .unwrap();
        run.await.unwrap().unwrap();
    })
    .await
    .unwrap();
}
