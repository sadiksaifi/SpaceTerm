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

struct Fixture {
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
        #[cfg(target_os = "macos")]
        let parent = std::path::PathBuf::from("/private/tmp");
        #[cfg(target_os = "linux")]
        let parent = std::path::PathBuf::from(std::env::var_os("HOME").unwrap());
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
        let ports = ProductionListenerPorts::new();
        boundary.config.host.authority = Arc::new(Authority(pin));
        boundary.config.core =
            CorePorts::production(identity, home, boundary.dependencies.clock.clone()).unwrap();
        boundary.config.outcomes = ports.outcomes.clone();
        boundary.config.extensions = ports.extensions(boundary.config.extensions);
        boundary.dependencies.credentials = account.credentials.clone();
        boundary.dependencies.store = Box::new(account.store);
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
            spaceterm_protocol::negotiation::CAPABILITY_CANONICAL_DOMAIN,
            spaceterm_protocol::negotiation::CAPABILITY_ORGANIZATION_COMPARISONS,
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

#[tokio::test]
async fn production_ipc_and_wss_use_real_identity_pairing_and_reliable_actor_outcomes() {
    tokio::time::timeout(Duration::from_secs(30), async {
        for major in [1, 2] {
            let fixture = Fixture::new().await;
            let (streams, principal, device) = fixture.enroll(major).await;
            let (remote_streams, remote_principal, remote_device) =
                fixture.enroll_with_seed(major, 81).await;
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
            if let Some(ipv6) = addresses.get(1) {
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
                    handshake(&transport, pin, remote_principal, DeviceKey::new(82), major).await;
                assert_eq!(
                    query_status(&transport, &mut ipv6_codec)
                        .await
                        .parts()
                        .server,
                    ServerId::new(pin.identity)
                );
                transport.close().await.unwrap();
            }
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
    assert_eq!(
        result.unwrap_err().class,
        ServerFailureClass::RecoveryRequired
    );
    assert!(!endpoint.path().exists());
    assert_eq!(handle.phase(), ServicePhase::RecoveryRequired);
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
