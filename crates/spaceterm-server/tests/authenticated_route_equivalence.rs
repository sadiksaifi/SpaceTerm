//! Real IPC, TLS WebSocket and OpenSSH forwarding share the paired actor path.
//! Production listeners, pairing, SQLite, reducer and native PTYs stay real.
//! Account registration, startup and shutdown authorization are injected. No
//! service is installed and no real account configuration is changed.
#![cfg(any(target_os = "macos", target_os = "linux"))]

use ring::signature::{Ed25519KeyPair, KeyPair};
use spaceterm_core::{
    authority::*, content_queries::*, control::*, identity::*, management::*,
    operation_ledger::canonical::CanonicalCodec, operation_ledger_contracts::*, operations::*,
    organization::*, pairing::*, remote_access::RemoteAccessPolicy,
};
use spaceterm_host::{
    account_service::test_support::AccountServiceFixture,
    ipc::{IpcEndpoint, IpcTransport, PrivateIpcDirectory},
    native_deployment::contracts::NativeRequestId,
    organization_store::{
        SqliteOrganizationStore, StoreIdentity, StoreLimits, bootstrap::*, migrations, schema,
    },
    ssh_tunnel::{
        AuthenticationMode, RemoteSocketPath, SshDestination, SshEnvironment, SshTunnel,
        SshTunnelCommand,
    },
    startup::{BuildIdentity, DesiredServerState},
    storage::Database,
    transport::{MessageLimit, MessageTransport},
    wss::WssTransport,
};
use spaceterm_protocol::{
    ProtocolError,
    authentication::{self, ClientHandshake, Established, HandshakeConfig, Purpose, SigningKey},
    codec::Payload,
    connection::AuthenticatedConnection,
    convert::{content, identity, management, operations, runtime},
    pairing_adapter::*,
    wire::{v1, v2},
};
use spaceterm_server::{
    account_startup::AccountStartup,
    adapters::{
        core::CorePorts,
        store::production_services,
        terminal::{AccountLaunchFacts, AccountLaunchProvider, NativeRuntime, NativeRuntimeClock},
    },
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
    collections::BTreeMap,
    ffi::OsString,
    fs,
    net::SocketAddr,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command as NativeCommand, Stdio},
    sync::Arc,
    time::Duration,
};

#[path = "support/job_a.rs"]
mod native;
#[path = "support/job_b.rs"]
mod transport;

// Bounds test coordination only, not protocol admission or product deadlines.
const HARNESS_TIMEOUT: Duration = Duration::from_secs(180);
// OpenSSH startup uses the transport's valid deadline, not content-test coordination.
const SSH_STARTUP_TIMEOUT: Duration = Duration::from_secs(60);

use spaceterm_terminal::{runtime::budget::RuntimeLimits, runtime_contracts::*};

struct Key(Ed25519KeyPair);
impl Key {
    fn new(seed: u8) -> Self {
        Self(Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).unwrap())
    }
}
impl SigningKey for Key {
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
struct Account(PathBuf, u32);
impl AccountLaunchProvider for Account {
    fn account(&self) -> Result<AccountLaunchFacts, ServerFailure> {
        Ok(AccountLaunchFacts {
            uid: self.1,
            home: self.0.clone(),
            planner: spaceterm_host::shell_launch::ShellLaunchPlanner::for_test(
                PathBuf::from("/bin/sh"),
                self.0.clone(),
            ),
            machine: spaceterm_terminal::metadata::LocalMachine::default(),
        })
    }
}
struct Enrollment {
    server: RecoveredServer,
    service: PairingService,
    identity: Arc<spaceterm_host::server_keys::ServerIdentity>,
}
impl Enrollment {
    async fn enroll(
        &self,
        major: u32,
        seed: u8,
    ) -> (PairingClientStreams, authentication::Principal, Arc<Key>) {
        let device = Arc::new(Key::new(seed));
        let auth = Arc::new(Key::new(seed + 1));
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
        let claim = VerifiedDeviceClaim::verify(proposal, &proof_device, &proof_auth).unwrap();
        let issued = self
            .service
            .issue_owner_authorization(
                NativeRequestId::new([seed; 16]).unwrap(),
                RequestDigest::from_bytes([seed; 32]),
                claim,
            )
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
struct Fixture {
    _root: tempfile::TempDir,
    ipc: transport::Directory,
    endpoint: IpcEndpoint,
    handle: ServerHandle,
    running:
        tokio::task::JoinHandle<Result<spaceterm_server::shutdown::ShutdownReport, ServerFailure>>,
    stop: Arc<native::Host>,
    streams: [OperationStreamId; 2],
    principals: [authentication::Principal; 2],
    pin: authentication::ServerPin,
}
impl Fixture {
    async fn new(major: u32) -> Self {
        #[cfg(target_os = "macos")]
        let parent = PathBuf::from("/private/tmp");
        // Keep real SQLite synchronization off the shared Linux build filesystem.
        #[cfg(target_os = "linux")]
        let parent = PathBuf::from("/var/tmp");
        let root = tempfile::tempdir_in(parent).unwrap();
        let native_account =
            AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registration = native_account
            .initialize(DesiredServerState::Running)
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
        let database = Database::open_for_application(
            registration.organization_directory(),
            migrations(),
            schema::APPLICATION_ID,
        )
        .unwrap();
        match SqliteOrganizationStore::initialize_new(
            database,
            NewStoreSeed {
                identity,
                origin: NewServerOrigin::LocalHeadless,
            },
            StoreLimits::production(),
            production_services(identity, home.clone()).unwrap(),
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
        let clock = Arc::new(NativeRuntimeClock::default());
        let runtime = NativeRuntime::new(
            identity.server,
            identity.account.account_uid,
            Arc::new(Account(
                root.path().to_path_buf(),
                identity.account.account_uid,
            )),
            clock.clone(),
            RuntimeLimits::default(),
        )
        .unwrap();
        boundary.config.host.authority = Arc::new(Authority(pin));
        boundary.config.core = CorePorts::production(identity, home, clock.clone()).unwrap();
        boundary.config.outcomes = ports.outcomes.clone();
        boundary.config.extensions = ports.extensions(boundary.config.extensions);
        boundary.config.launch = runtime.clone();
        boundary.config.cleanup = runtime.clone();
        boundary.dependencies.runtime = runtime;
        boundary.dependencies.clock = clock;
        boundary.dependencies.credentials = account.credentials.clone();
        boundary.dependencies.store = Box::new(account.store);
        let server = RecoveredServer::recover(boundary.config, boundary.dependencies)
            .await
            .unwrap();
        let handle = server.handle();
        let service = PairingService::new(
            Arc::new(handle.clone()),
            account.credentials.clone(),
            Arc::new(CanonicalCodec),
            Arc::new(Owner),
            pin,
        )
        .unwrap();
        let enrollment = Enrollment {
            server,
            service,
            identity: account.credentials.clone(),
        };
        let (first, first_principal, _) = enrollment.enroll(major, 42).await;
        let (second, second_principal, _) = enrollment.enroll(major, 44).await;
        let endpoint = account.registration.endpoint().unwrap();
        let factories = production_listener_factories(
            handle.clone(),
            ProductionListenerConfig {
                registration: &account.registration,
                identity: account.credentials,
                running: spaceterm_distribution::BuildVersion::parse("1.2.3").unwrap(),
                codec: Arc::new(CanonicalCodec),
                pairing: enrollment.service,
                ports,
            },
            Arc::new(PairingProtocolLookup::new(handle.clone())),
        )
        .unwrap();
        let running = tokio::spawn(enrollment.server.run(factories));
        tokio::time::timeout(HARNESS_TIMEOUT, async {
            while !endpoint.path().exists() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        Self {
            _root: root,
            ipc: transport::Directory::new(),
            endpoint,
            handle,
            running,
            stop,
            streams: [first.parts().ordinary, second.parts().ordinary],
            principals: [first_principal, second_principal],
            pin,
        }
    }
    fn principal(&self, index: usize) -> authentication::Principal {
        self.principals[index]
    }
    async fn shutdown(self) {
        self.handle
            .shutdown(TrustedShutdownRequest {
                authority: self.stop,
            })
            .await
            .unwrap();
        tokio::time::timeout(HARNESS_TIMEOUT, self.running)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(!self.endpoint.path().exists());
    }
}

#[derive(Clone, Copy, Debug)]
enum Route {
    Ipc,
    Wss,
    Ssh,
    Jump,
}
struct Routes<'a> {
    fixture: &'a Fixture,
    route: Route,
    wss: Option<SocketAddr>,
    ssh: Option<OwnedSsh>,
    next_endpoint: u64,
}
impl Routes<'_> {
    async fn open(&mut self) -> OpenRoute {
        match self.route {
            Route::Ipc => OpenRoute {
                transport: Arc::new(
                    IpcTransport::connect(&self.fixture.endpoint, MessageLimit::envelope())
                        .await
                        .unwrap(),
                ),
                tunnel: None,
            },
            Route::Wss => OpenRoute {
                transport: Arc::new(
                    WssTransport::connect(
                        self.wss.unwrap(),
                        "localhost",
                        spaceterm_host::transport::EnrolledServerKey(self.fixture.pin.public_key),
                        MessageLimit::envelope(),
                        Duration::from_secs(10),
                    )
                    .await
                    .unwrap(),
                ),
                tunnel: None,
            },
            Route::Ssh | Route::Jump => {
                self.next_endpoint += 1;
                let ssh = self.ssh.as_ref().unwrap();
                let endpoint = self
                    .fixture
                    .ipc
                    .directory
                    .endpoint(&format!("f{}", self.next_endpoint))
                    .unwrap();
                let command = SshTunnelCommand::new(
                    PathBuf::from("/usr/bin/ssh"),
                    ssh.client_config.clone(),
                    SshDestination::new(if matches!(self.route, Route::Jump) {
                        "target-jump"
                    } else {
                        "target"
                    })
                    .unwrap(),
                    endpoint,
                    RemoteSocketPath::new(self.fixture.endpoint.path().to_str().unwrap()).unwrap(),
                    AuthenticationMode::BatchMode,
                )
                .unwrap();
                let tunnel = SshTunnel::start(
                    command,
                    SshEnvironment::new(
                        ssh.directory.path.clone(),
                        vec![(OsString::from("PATH"), OsString::from("/usr/bin:/bin"))],
                    )
                    .unwrap(),
                    None,
                    SSH_STARTUP_TIMEOUT,
                )
                .await
                .unwrap();
                let transport = IpcTransport::connect(tunnel.endpoint(), MessageLimit::envelope())
                    .await
                    .unwrap();
                OpenRoute {
                    transport: Arc::new(transport),
                    tunnel: Some(tunnel),
                }
            }
        }
    }
}
struct OpenRoute {
    transport: Arc<dyn MessageTransport>,
    tunnel: Option<SshTunnel>,
}
impl OpenRoute {
    async fn close(self) {
        self.transport.close().await.unwrap();
        if let Some(tunnel) = self.tunnel {
            let endpoint = tunnel.endpoint().path().to_path_buf();
            let pid = tunnel.process_id().expect("owned tunnel has a PID");
            tokio::time::timeout(HARNESS_TIMEOUT, tunnel.cancel())
                .await
                .unwrap()
                .unwrap();
            assert!(
                !endpoint.exists(),
                "forwarding socket must be removed after cleanup"
            );
            // waitpid-based tunnel completion already proves reaping. A second
            // external check catches a supervisor that acknowledges too early.
            let status = NativeCommand::new("/bin/kill")
                .args(["-0", &pid.to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap();
            assert!(
                !status.success(),
                "exact owned tunnel process survived cleanup"
            );
        }
    }
}
struct Client {
    route: OpenRoute,
    codec: AuthenticatedConnection,
    stream: OperationStreamId,
    sequence: u64,
    last_frame: Vec<u8>,
    generation: Option<ConnectionGeneration>,
    runtime_request: u64,
    rows: BTreeMap<spaceterm_core::content::RowId, String>,
    text: String,
    content_stamp: Option<QueryStamp>,
    metadata: Vec<RuntimeMetadataSnapshot>,
    metadata_assembler: spaceterm_protocol::convert::metadata::MetadataAssembler,
    metadata_fragments: usize,
    base: Option<spaceterm_core::content::ContentBase>,
    row_revisions: BTreeMap<spaceterm_core::content::RowId, u64>,
    mouse_enabled: bool,
}
impl Client {
    async fn connect(routes: &mut Routes<'_>, index: usize, major: u32) -> Self {
        Self::connect_with_limits(routes, index, major, Default::default()).await
    }
    async fn connect_with_limits(
        routes: &mut Routes<'_>,
        index: usize,
        major: u32,
        limits: spaceterm_protocol::limits::NegotiatedLimits,
    ) -> Self {
        let route = routes.open().await;
        let codec = handshake_with_limits(
            &route,
            routes.fixture.pin,
            routes.fixture.principal(index),
            major,
            43 + index as u8 * 2,
            limits,
        )
        .await
        .unwrap();
        assert_eq!(codec.major(), major);
        assert!(
            codec
                .capabilities()
                .contains(&spaceterm_protocol::negotiation::CAPABILITY_TERMINAL_STREAMS),
            "production listeners must negotiate the terminal context and subscription contract"
        );
        Self {
            route,
            codec,
            stream: routes.fixture.streams[index],
            sequence: 1,
            last_frame: Vec::new(),
            generation: None,
            runtime_request: 0,
            rows: BTreeMap::new(),
            text: String::new(),
            content_stamp: None,
            metadata: Vec::new(),
            metadata_assembler: Default::default(),
            metadata_fragments: 0,
            base: None,
            row_revisions: BTreeMap::new(),
            mouse_enabled: false,
        }
    }
    fn request(&mut self, command: Command) -> CanonicalOperationRequest {
        let request = CanonicalOperationRequest::new(
            OperationId::new(self.stream, OperationSequence::new(self.sequence).unwrap()),
            command,
            &CanonicalCodec,
        )
        .unwrap();
        self.sequence += 1;
        request
    }
    async fn send(&mut self, payload: Payload) {
        self.last_frame = self.codec.encode(&payload).unwrap();
        self.route.transport.send(&self.last_frame).await.unwrap();
    }
    async fn recv(&mut self) -> Payload {
        let bytes = tokio::time::timeout(HARNESS_TIMEOUT, self.route.transport.recv())
            .await
            .unwrap()
            .unwrap()
            .expect("authenticated response");
        assert!(bytes.len() <= self.codec.limits().frame_bytes);
        let payload = self.codec.decode(&bytes).unwrap();
        macro_rules! record {
            ($publication:expr, $p:ident) => {{
                let publication = $publication;
                if let Some($p::ContentReadResponse {
                    result: Some($p::content_read_response::Result::Page(page)),
                }) = &publication.response
                {
                    if publication.index == 0 {
                        self.rows.clear();
                    }
                    let stamp = content::$p::decode_stamp(page.stamp.as_ref().unwrap()).unwrap();
                    self.content_stamp = Some(stamp);
                    self.base = Some(stamp.base());
                    let request = ContentPageRequest::new(
                        stamp,
                        ContentRange::ActiveGrid,
                        None,
                        PageLimits::new(256, 16384, 1024 * 1024).unwrap(),
                    )
                    .unwrap();
                    let decoded = content::$p::decode_page(page, &request).unwrap();
                    self.mouse_enabled = decoded.page().parts().modes.mouse_tracking
                        != spaceterm_core::content::MouseTracking::None;
                    for row in &decoded.page().parts().rows {
                        self.row_revisions.insert(row.id(), row.revision());
                        let text: String = row
                            .cells()
                            .iter()
                            .map(|cell| cell.content().text())
                            .collect();
                        if row.start_column() == 0 {
                            self.rows.insert(row.id(), text);
                        } else {
                            self.rows
                                .get_mut(&row.id())
                                .expect("row prefix")
                                .push_str(&text);
                        }
                    }
                    if publication.complete {
                        self.text = self.rows.values().cloned().collect::<Vec<_>>().join("\n");
                    }
                }
            }};
        }
        match &payload {
            Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::ContentPublication(value)),
            }) => record!(value, v1),
            Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::ContentPublication(value)),
            }) => record!(value, v2),
            Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::MetadataPublication(value)),
            }) => {
                let (_, snapshot) =
                    spaceterm_protocol::convert::metadata::v1::decode_publication(value).unwrap();
                self.metadata.push(snapshot);
            }
            Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::MetadataPublication(value)),
            }) => {
                let (_, snapshot) =
                    spaceterm_protocol::convert::metadata::v2::decode_publication(value).unwrap();
                self.metadata.push(snapshot);
            }
            Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::MetadataFragment(value)),
            }) => {
                let fragment =
                    spaceterm_protocol::convert::metadata::v1::decode_fragment(value).unwrap();
                self.metadata_fragments += 1;
                if let Some((_, snapshot)) = self.metadata_assembler.receive(fragment).unwrap() {
                    self.metadata.push(snapshot);
                }
            }
            Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::MetadataFragment(value)),
            }) => {
                let fragment =
                    spaceterm_protocol::convert::metadata::v2::decode_fragment(value).unwrap();
                self.metadata_fragments += 1;
                if let Some((_, snapshot)) = self.metadata_assembler.receive(fragment).unwrap() {
                    self.metadata.push(snapshot);
                }
            }
            _ => {}
        }
        payload
    }
    async fn response(&mut self) -> Payload {
        loop {
            let payload = self.recv().await;
            if !matches!(
                &payload,
                Payload::Major1(v1::Message {
                    kind: Some(
                        v1::message::Kind::ContentPublication(_)
                            | v1::message::Kind::MetadataPublication(_)
                            | v1::message::Kind::MetadataFragment(_)
                    )
                }) | Payload::Major2(v2::Message {
                    kind: Some(
                        v2::message::Kind::ContentPublication(_)
                            | v2::message::Kind::MetadataPublication(_)
                            | v2::message::Kind::MetadataFragment(_)
                    )
                })
            ) {
                return payload;
            }
        }
    }
    async fn exchange(&mut self, payload: Payload) -> Payload {
        self.send(payload).await;
        self.response().await
    }
    async fn status(&mut self) -> ServerStatus {
        macro_rules! payload {
            ($p:ident, $major:ident) => {
                Payload::$major($p::Message {
                    kind: Some($p::message::Kind::StatusQuery($p::StatusQuery {
                        request: 7,
                        request_generation: 1,
                        ..Default::default()
                    })),
                })
            };
        }
        let request = if self.codec.major() == 1 {
            payload!(v1, Major1)
        } else {
            payload!(v2, Major2)
        };
        let publication = match self.exchange(request).await {
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
            other => panic!("status publication: {other:?}"),
        };
        let StatusPublication::Snapshot { status, .. } = publication else {
            panic!("status snapshot")
        };
        status
    }
    async fn context(&mut self, pane: PaneId) {
        macro_rules! payload {
            ($p:ident, $major:ident) => {
                Payload::$major($p::Message {
                    kind: Some($p::message::Kind::ClientContext($p::ClientContext {
                        request: 8,
                        epoch: self.codec.epoch(),
                        connection_generation: self.generation.unwrap().get(),
                        context_generation: 2,
                        focused_pane: Some(identity::encode_pane(pane)),
                        terminal_input_focus: true,
                        native_effects_eligible: true,
                        acknowledged: false,
                        failure: None,
                    })),
                })
            };
        }
        let request = if self.codec.major() == 1 {
            payload!(v1, Major1)
        } else {
            payload!(v2, Major2)
        };
        match self.exchange(request).await {
            Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::ClientContext(value)),
            }) => assert!(
                value.acknowledged,
                "context update rejected: {:?}",
                value.failure
            ),
            Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::ClientContext(value)),
            }) => assert!(
                value.acknowledged,
                "context update rejected: {:?}",
                value.failure
            ),
            other => panic!("context acknowledgement: {other:?}"),
        }
    }
    fn fence(&self, session: SessionKey, grant: ControlGrant) -> EffectFence {
        EffectFence::new(
            session,
            grant,
            ConnectionBinding::new(self.stream.client(), self.generation.unwrap()),
            ContextGeneration::new(2),
            None,
        )
        .unwrap()
    }
    async fn start(&mut self, handle: &ServerHandle, grant: ControlGrant) -> SessionKey {
        self.context(grant.pane()).await;
        let snapshot = handle.snapshot();
        let before = SessionKey {
            pane: grant.pane(),
            epoch: snapshot.epoch(),
            generation: snapshot
                .runtime()
                .iter()
                .find(|runtime| runtime.pane == grant.pane())
                .unwrap()
                .session_generation,
        };
        let fence = self.fence(before, grant);
        let id = OperationId::new(self.stream, OperationSequence::new(self.sequence).unwrap());
        let intent = StartupIntent::new(id, fence, StartupSource::ExplicitSelection).unwrap();
        assert!(matches!(
            self.command(Command::BeginStartup(intent)).await,
            Outcome::StartupAwaitingMeasurement { .. }
        ));
        let grid = spaceterm_core::geometry::MeasuredGrid::new(
            24,
            100,
            spaceterm_core::geometry::CellPixels::new(9, 18).unwrap(),
            spaceterm_core::geometry::SUPPORTED_GRID_LIMITS,
        )
        .unwrap();
        let Outcome::SessionStarted(session) = self
            .command(Command::CompleteMeasuredStartup(
                MeasuredStartupMeasurement {
                    intent: intent.id(),
                    fence,
                    grid,
                },
            ))
            .await
        else {
            panic!("session started")
        };
        let mut snapshots = handle.subscribe_snapshot();
        tokio::time::timeout(HARNESS_TIMEOUT, async {
            while !snapshots.borrow().runtime().iter().any(|runtime| runtime.pane == grant.pane() && matches!(runtime.phase, SessionPhase::Running { generation } if generation == session.generation)) { snapshots.changed().await.unwrap(); }
        }).await.unwrap();
        session
    }
    async fn subscribe(&mut self, fence: EffectFence) {
        self.subscribe_with_limits(fence, PageLimits::new(4, 1024, 16384).unwrap())
            .await;
    }
    async fn subscribe_with_limits(&mut self, fence: EffectFence, limits: PageLimits) {
        macro_rules! payload {
            ($p:ident, $major:ident) => {
                Payload::$major($p::Message {
                    kind: Some($p::message::Kind::PaneSubscription($p::PaneSubscription {
                        request: 9,
                        request_generation: 1,
                        fence: Some(identity::encode_fence(fence)),
                        cancel: false,
                        limits: Some($p::PageLimits {
                            rows: u32::from(limits.rows()),
                            cells: limits.cells(),
                            bytes: limits.bytes(),
                        }),
                    })),
                })
            };
        }
        self.send(if self.codec.major() == 1 {
            payload!(v1, Major1)
        } else {
            payload!(v2, Major2)
        })
        .await;
    }
    async fn read_query(&mut self, query: &content::reads::ReadQuery) {
        self.send(if self.codec.major() == 1 {
            Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::ContentQuery(
                    content::reads::v1::encode_query(query),
                )),
            })
        } else {
            Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::ContentQuery(
                    content::reads::v2::encode_query(query),
                )),
            })
        })
        .await;
    }
    async fn authenticated_content_reads(&mut self) {
        use spaceterm_core::content::CellRef;
        let old = self.content_stamp.expect("content publication");
        let input_fence = EffectFence::new(
            old.fence().session(),
            old.fence().grant(),
            old.fence().connection(),
            old.fence().context(),
            None,
        )
        .unwrap();
        self.line(input_fence, "stty -echo; i=0; while [ $i -lt 10000 ]; do printf 'full-history-%05d-abcdefghijklmnopqrstuvwxyz0123456789\\n' \"$i\"; i=$((i + 1)); done; printf 'history-complete\\n'").await;
        self.output("history-complete").await;
        let old = self.content_stamp.unwrap();
        let mut stamp = QueryStamp::new(
            old.fence(),
            old.base(),
            ContentRequestId::new(101).unwrap(),
            RequestGeneration::new(1),
        )
        .unwrap();
        let limits = PageLimits::new(256, 16384, 128 * 1024).unwrap();
        let mut rows = Vec::new();
        let mut after = None;
        let mut retries = 0;
        loop {
            let request = TopologyPageRequest::new(stamp, after, limits).unwrap();
            self.read_query(&content::reads::ReadQuery::Topology(request))
                .await;
            let response = self.response().await;
            macro_rules! topology {
                ($value:expr, $p:ident) => {{
                    match $value.result.unwrap() {
                        $p::content_read_response::Result::Topology(page) => {
                            assert_eq!(
                                content::$p::decode_stamp(page.stamp.as_ref().unwrap()).unwrap(),
                                stamp
                            );
                            rows.extend(
                                page.rows.iter().map(|row| {
                                    content::reads::$p::decode_topology_row(row).unwrap()
                                }),
                            );
                            after = page.after.as_ref().map(|row| {
                                TopologyContinuation::new(
                                    stamp,
                                    content::reads::$p::decode_topology_row(row).unwrap(),
                                )
                                .unwrap()
                            });
                            if after.is_none() {
                                break;
                            }
                        }
                        $p::content_read_response::Result::ResyncRequired(value) => {
                            assert_eq!(
                                content::$p::decode_stamp(value.stamp.as_ref().unwrap()).unwrap(),
                                stamp
                            );
                            let base =
                                content::$p::decode_base(value.current.as_ref().unwrap()).unwrap();
                            let fence = EffectFence::new(
                                stamp.fence().session(),
                                stamp.fence().grant(),
                                stamp.fence().connection(),
                                stamp.fence().context(),
                                Some(base.revision),
                            )
                            .unwrap();
                            stamp = QueryStamp::new(
                                fence,
                                base,
                                stamp.request(),
                                stamp.generation().checked_successor().unwrap(),
                            )
                            .unwrap();
                            after = None;
                            rows.clear();
                            retries += 1;
                            assert!(retries < 20);
                        }
                        $p::content_read_response::Result::Rejected(value) if value.reason == 1 => {
                            let old = self.content_stamp.unwrap();
                            stamp = QueryStamp::new(
                                old.fence(),
                                old.base(),
                                stamp.request(),
                                stamp.generation().checked_successor().unwrap(),
                            )
                            .unwrap();
                            after = None;
                            rows.clear();
                            retries += 1;
                            assert!(retries < 20);
                        }
                        other => panic!("topology response: {other:?}"),
                    }
                }};
            }
            match response {
                Payload::Major1(v1::Message {
                    kind: Some(v1::message::Kind::ContentReadResponse(value)),
                }) => topology!(value, v1),
                Payload::Major2(v2::Message {
                    kind: Some(v2::message::Kind::ContentReadResponse(value)),
                }) => topology!(value, v2),
                other => panic!("topology response: {other:?}"),
            }
        }
        assert!(
            rows.len() >= 10000,
            "full supported Scrollback plus active grid"
        );
        let endpoint = |row: TopologyRow, column| CellRef {
            session: stamp.base().session,
            row: row.row(),
            row_revision: row.revision(),
            column,
        };
        let first = rows[0];
        let last = *rows.last().unwrap();
        let copy = CopyRequest::new(
            stamp,
            ContentSelection::new(
                stamp,
                endpoint(first, 0),
                endpoint(last, last.columns() - 1),
                false,
            )
            .unwrap(),
            CopyOptions {
                unwrap_soft_wraps: true,
                trim_trailing_spaces: true,
                include_html: false,
            },
        )
        .unwrap();
        let (plain, html, copy_chunks) = self.copy_bytes(copy).await;
        assert!(html.is_empty());
        assert!(plain.len() > MAX_RESOURCE_CHUNK_BYTES);
        assert!(copy_chunks > 2);
        let plain = String::from_utf8(plain).unwrap();
        let history_lines = plain
            .lines()
            .filter(|line| line.starts_with("full-history-"))
            .count();
        let expected_matches = plain.matches("full-history-").count();
        assert!(
            history_lines >= 9990,
            "copy covers retained history, not just the active grid"
        );
        assert!(plain.contains("full-history-09999-abcdefghijklmnopqrstuvwxyz0123456789"));
        let find = FindRequest::new(
            stamp,
            BoundedFindQuery::new("full-history-".into()).unwrap(),
            None,
        )
        .unwrap();
        self.read_query(&content::reads::ReadQuery::Find(find))
            .await;
        let mut found = 0;
        let mut chunks = 0;
        loop {
            macro_rules! find_event {
                ($value:expr, $p:ident) => {{
                    match $value.item.unwrap() {
                        $p::find_event::Item::Chunk(chunk) => {
                            assert_eq!(
                                content::$p::decode_stamp(chunk.stamp.as_ref().unwrap()).unwrap(),
                                stamp
                            );
                            assert_eq!(chunk.index, chunks);
                            chunks += 1;
                            assert!(chunk.matches.len() <= MAX_FIND_CHUNK_MATCHES);
                            for value in &chunk.matches {
                                assert_eq!(
                                    content::reads::$p::decode_cell(value.start.as_ref().unwrap())
                                        .unwrap()
                                        .session,
                                    stamp.base().session
                                );
                            }
                            found += chunk.matches.len();
                        }
                        $p::find_event::Item::Complete(value) => {
                            assert_eq!(value.chunks, chunks);
                            break;
                        }
                        other => panic!("find response: {other:?}"),
                    }
                }};
            }
            match self.response().await {
                Payload::Major1(v1::Message {
                    kind: Some(v1::message::Kind::FindEvent(value)),
                }) => find_event!(value, v1),
                Payload::Major2(v2::Message {
                    kind: Some(v2::message::Kind::FindEvent(value)),
                }) => find_event!(value, v2),
                other => panic!("find response: {other:?}"),
            }
        }
        assert_eq!(found, expected_matches);
        assert!(chunks > 1);

        // Full-Scrollback rich output uses the same authenticated stream and
        // validates both digests, including the final HTML chunk after coalescing.
        let rich_stamp = QueryStamp::new(
            stamp.fence(),
            stamp.base(),
            ContentRequestId::new(103).unwrap(),
            stamp.generation(),
        )
        .unwrap();
        let rich_end = last;
        let rich = CopyRequest::new(
            rich_stamp,
            ContentSelection::new(
                rich_stamp,
                endpoint(first, 0),
                endpoint(rich_end, rich_end.columns() - 1),
                false,
            )
            .unwrap(),
            CopyOptions {
                include_html: true,
                ..copy.options()
            },
        )
        .unwrap();
        let (rich_plain, rich_html, _) = self.copy_bytes(rich).await;
        assert_eq!(String::from_utf8(rich_plain).unwrap(), plain);
        let rich_html = String::from_utf8(rich_html).unwrap();
        assert!(rich_html.contains("<div"));
        assert!(rich_html.contains("full-history-09999-abcdefghijklmnopqrstuvwxyz0123456789"));
        assert!(rich_html.matches("full-history-").count() >= expected_matches);

        let cancelled = QueryStamp::new(
            stamp.fence(),
            stamp.base(),
            stamp.request(),
            stamp.generation().checked_successor().unwrap(),
        )
        .unwrap();
        let cancelled_copy = CopyRequest::new(cancelled, copy.selection(), copy.options()).unwrap();
        self.read_query(&content::reads::ReadQuery::Copy(cancelled_copy))
            .await;
        self.read_query(&content::reads::ReadQuery::Cancel {
            stamp: cancelled,
            kind: 5,
        })
        .await;
        self.copy_failure(cancelled, 2).await;
        // Cancellation leaves the same authenticated connection and Session usable.
        assert_eq!(
            self.status().await.parts().epoch,
            stamp.base().session.epoch
        );

        let mut wrong_base = stamp.base();
        wrong_base.revision = wrong_base.revision.checked_successor().unwrap();
        let fence = EffectFence::new(
            stamp.fence().session(),
            stamp.fence().grant(),
            stamp.fence().connection(),
            stamp.fence().context(),
            Some(wrong_base.revision),
        )
        .unwrap();
        let stale = QueryStamp::new(
            fence,
            wrong_base,
            ContentRequestId::new(102).unwrap(),
            stamp.generation(),
        )
        .unwrap();
        self.read_query(&content::reads::ReadQuery::Copy(
            CopyRequest::new(stale, copy.selection(), copy.options()).unwrap(),
        ))
        .await;
        self.copy_failure(stale, 1).await;
        assert_eq!(
            self.status().await.parts().epoch,
            stamp.base().session.epoch
        );
    }
    async fn copy_bytes(&mut self, copy: CopyRequest) -> (Vec<u8>, Vec<u8>, usize) {
        use sha2::{Digest, Sha256};
        let stamp = copy.stamp();
        self.read_query(&content::reads::ReadQuery::Copy(copy))
            .await;
        let mut plain = Vec::new();
        let mut html = Vec::new();
        let mut copy_chunks = 0;
        loop {
            macro_rules! copy_event {
                ($value:expr, $p:ident) => {{
                    match $value.item.unwrap() {
                        $p::copy_event::Item::Chunk(chunk) => {
                            assert_eq!(
                                content::$p::decode_stamp(chunk.stamp.as_ref().unwrap()).unwrap(),
                                stamp
                            );
                            let bytes = if chunk.part == 1 {
                                &mut plain
                            } else {
                                assert_eq!(chunk.part, 2);
                                &mut html
                            };
                            assert_eq!(chunk.offset, bytes.len() as u64);
                            assert!(chunk.data.len() <= MAX_RESOURCE_CHUNK_BYTES);
                            bytes.extend(chunk.data);
                            copy_chunks += 1;
                        }
                        $p::copy_event::Item::Complete(value) => {
                            assert_eq!(
                                content::$p::decode_stamp(value.stamp.as_ref().unwrap()).unwrap(),
                                stamp
                            );
                            assert_eq!(value.plain_bytes, plain.len() as u64);
                            assert_eq!(value.plain_digest, Sha256::digest(&plain).to_vec());
                            assert_eq!(
                                value.html_bytes,
                                copy.options().include_html.then_some(html.len() as u64)
                            );
                            assert_eq!(
                                value.html_digest,
                                copy.options()
                                    .include_html
                                    .then(|| Sha256::digest(&html).to_vec())
                            );
                            break;
                        }
                        other => panic!("copy response: {other:?}"),
                    }
                }};
            }
            match self.response().await {
                Payload::Major1(v1::Message {
                    kind: Some(v1::message::Kind::CopyEvent(value)),
                }) => copy_event!(value, v1),
                Payload::Major2(v2::Message {
                    kind: Some(v2::message::Kind::CopyEvent(value)),
                }) => copy_event!(value, v2),
                other => panic!("copy response: {other:?}"),
            }
        }
        (plain, html, copy_chunks)
    }
    async fn copy_failure(&mut self, stamp: QueryStamp, reason: i32) {
        let response = self.response().await;
        macro_rules! check {
            ($value:expr, $p:ident) => {{
                let $p::copy_event::Item::ContentFailure(value) = $value.item.unwrap() else {
                    panic!("copy must not complete after cancellation or stale content")
                };
                assert_eq!(
                    content::$p::decode_stamp(value.stamp.as_ref().unwrap()).unwrap(),
                    stamp
                );
                assert_eq!(value.reason, reason);
            }};
        }
        match response {
            Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::CopyEvent(value)),
            }) => check!(value, v1),
            Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::CopyEvent(value)),
            }) => check!(value, v2),
            other => panic!("copy failure: {other:?}"),
        }
    }
    async fn effect(&mut self, fence: EffectFence, command: RuntimeCommand) -> Option<i32> {
        self.runtime_request += 1;
        let request = self.runtime_request;
        let command = FencedRuntimeCommand { fence, command };
        let payload = if self.codec.major() == 1 {
            Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::RuntimeCommand(
                    runtime::v1::encode_command(request, &command).unwrap(),
                )),
            })
        } else {
            Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::RuntimeCommand(
                    runtime::v2::encode_command(request, &command).unwrap(),
                )),
            })
        };
        match self.exchange(payload).await {
            Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::RuntimeOutcome(value)),
            }) => {
                assert_eq!(value.request, request);
                value.failure.map(|failure| failure.classification)
            }
            Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::RuntimeOutcome(value)),
            }) => {
                assert_eq!(value.request, request);
                value.failure.map(|failure| failure.classification)
            }
            other => panic!("runtime outcome: {other:?}"),
        }
    }
    async fn line(&mut self, fence: EffectFence, text: &str) {
        assert_eq!(
            self.effect(
                fence,
                RuntimeCommand::Key(spaceterm_terminal::KeyInput::text_input(text))
            )
            .await,
            None
        );
        let mut enter = spaceterm_terminal::KeyInput::text_input("");
        enter.physical_key = spaceterm_terminal::PhysicalKey::Enter;
        assert_eq!(self.effect(fence, RuntimeCommand::Key(enter)).await, None);
    }
    async fn output(&mut self, expected: &str) {
        tokio::time::timeout(HARNESS_TIMEOUT, async {
            while !self.text.lines().any(|line| line.trim() == expected) {
                self.recv().await;
            }
        })
        .await
        .expect("semantic subscription reaches expected shell output");
    }
    async fn submit(&mut self, request: &CanonicalOperationRequest) -> Outcome {
        let payload = match self.codec.major() {
            1 => Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::Operation(
                    operations::v1::encode_request(request).unwrap(),
                )),
            }),
            _ => Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::Operation(
                    operations::v2::encode_request(request).unwrap(),
                )),
            }),
        };
        tokio::time::timeout(HARNESS_TIMEOUT, async {
            let mut payload = payload;
            loop {
                let result = match self.exchange(payload).await {
                    Payload::Major1(v1::Message {
                        kind: Some(v1::message::Kind::Outcome(value)),
                    }) => {
                        return operations::v1::decode_outcome(&value, request.binding().id)
                            .unwrap()
                            .1;
                    }
                    Payload::Major2(v2::Message {
                        kind: Some(v2::message::Kind::Outcome(value)),
                    }) => {
                        return operations::v2::decode_outcome(&value, request.binding().id)
                            .unwrap()
                            .1;
                    }
                    Payload::Major1(v1::Message {
                        kind: Some(v1::message::Kind::OperationLookupResult(value)),
                    }) => operations::v1::decode_lookup_result(&value, request.binding()).unwrap(),
                    Payload::Major2(v2::Message {
                        kind: Some(v2::message::Kind::OperationLookupResult(value)),
                    }) => operations::v2::decode_lookup_result(&value, request.binding()).unwrap(),
                    other => panic!("ordinary operation result: {other:?}"),
                };
                match result {
                    OperationLookup::Completed { result, .. } => return result,
                    OperationLookup::Pending => {}
                    other => panic!("live identified operation: {other:?}"),
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
                payload = if self.codec.major() == 1 {
                    Payload::Major1(v1::Message {
                        kind: Some(v1::message::Kind::OperationLookupRequest(
                            operations::v1::encode_lookup_request(request.binding()),
                        )),
                    })
                } else {
                    Payload::Major2(v2::Message {
                        kind: Some(v2::message::Kind::OperationLookupRequest(
                            operations::v2::encode_lookup_request(request.binding()),
                        )),
                    })
                };
            }
        })
        .await
        .expect("identified operation finishes within the test harness budget")
    }

    async fn command(&mut self, command: Command) -> Outcome {
        let request = self.request(command);
        self.submit(&request).await
    }
    async fn reconcile(&mut self, handle: &ServerHandle) -> ConnectionGeneration {
        macro_rules! reconciliation {
            ($v:ident, $major:ident) => {
                Payload::$major($v::Message {
                    kind: Some($v::message::Kind::Reconciliation($v::Reconciliation {
                        request: 1,
                        request_generation: 1,
                        server: Some(identity::encode_global(self.stream.server().bytes())),
                        epoch: self.codec.epoch(),
                        client: Some(identity::encode_client(self.stream.client())),
                        connection_generation: 0,
                        organization_revision: handle.snapshot().state().revision.get(),
                        acknowledged: false,
                        grid_limits: None,
                    })),
                })
            };
        }
        let payload = if self.codec.major() == 1 {
            reconciliation!(v1, Major1)
        } else {
            reconciliation!(v2, Major2)
        };
        let generation = match self.exchange(payload).await {
            Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::Reconciliation(value)),
            }) => {
                assert!(value.acknowledged);
                ConnectionGeneration::new(value.connection_generation)
            }
            Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::Reconciliation(value)),
            }) => {
                assert!(value.acknowledged);
                ConnectionGeneration::new(value.connection_generation)
            }
            _ => panic!("reconciliation acknowledgement"),
        };
        self.generation = Some(generation);
        generation
    }
}
async fn handshake(
    route: &OpenRoute,
    pin: authentication::ServerPin,
    principal: authentication::Principal,
    major: u32,
    seed: u8,
) -> Result<AuthenticatedConnection, ProtocolError> {
    handshake_with_limits(route, pin, principal, major, seed, Default::default()).await
}
async fn handshake_with_limits(
    route: &OpenRoute,
    pin: authentication::ServerPin,
    principal: authentication::Principal,
    major: u32,
    seed: u8,
    limits: spaceterm_protocol::limits::NegotiatedLimits,
) -> Result<AuthenticatedConnection, ProtocolError> {
    let (client, hello) = ClientHandshake::begin(
        pin,
        principal,
        Purpose::Ordinary,
        HandshakeConfig {
            majors: vec![major],
            limits,
            capabilities: vec![
                spaceterm_protocol::negotiation::CAPABILITY_CANONICAL_DOMAIN,
                spaceterm_protocol::negotiation::CAPABILITY_TERMINAL_STREAMS,
                spaceterm_protocol::negotiation::CAPABILITY_APPLICATION_MOUSE,
                spaceterm_protocol::negotiation::CAPABILITY_RUNTIME_METADATA,
                spaceterm_protocol::negotiation::CAPABILITY_RUNTIME_METADATA_FRAGMENTS,
            ],
        },
        Arc::new(Key::new(seed)),
    )?;
    route.transport.send(&hello).await.unwrap();
    let reply = tokio::time::timeout(HARNESS_TIMEOUT, route.transport.recv())
        .await
        .unwrap()
        .unwrap()
        .ok_or(ProtocolError::Authentication)?;
    let (client, proof) = client.receive_server(&reply)?;
    route.transport.send(&proof).await.unwrap();
    let ready = tokio::time::timeout(HARNESS_TIMEOUT, route.transport.recv())
        .await
        .unwrap()
        .unwrap()
        .ok_or(ProtocolError::Authentication)?;
    let Established::Ordinary(codec) = client.receive_ready(&ready)? else {
        panic!("ordinary handshake")
    };
    Ok(codec)
}

async fn enabled_status(routes: &mut Routes<'_>, major: u32) -> SocketAddr {
    let mut owner = Client::connect(routes, 0, major).await;
    owner.reconcile(&routes.fixture.handle).await;
    let endpoint = tokio::time::timeout(HARNESS_TIMEOUT, async {
        loop {
            if let Some(listener) = owner
                .status()
                .await
                .parts()
                .listeners
                .iter()
                .find(|listener| listener.state == ListenerState::Listening)
            {
                break listener.endpoint;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    owner.route.close().await;
    endpoint
}

// Each fixture owns two loopback sshd processes, keys and known_hosts. It uses
// normal host-key/account trust. MaxSessions=0 forbids SSH terminal/session
// channels; only forwarding remains available. No real account file is edited.
struct OwnedSsh {
    children: Vec<Child>,
    client_config: PathBuf,
    directory: transport::Directory,
}
impl OwnedSsh {
    async fn new() -> Self {
        for program in ["/usr/sbin/sshd", "/usr/bin/ssh", "/usr/bin/ssh-keygen"] {
            assert!(
                Path::new(program).is_file(),
                "BLOCKED: isolated SSH route prerequisite missing: {program}"
            );
        }
        let mut nonce = [0; 8];
        getrandom::fill(&mut nonce).unwrap();
        let path = PathBuf::from(
            std::env::var_os("HOME").expect("BLOCKED: SSH fixture needs account HOME"),
        )
        .join(format!("st-ssh{:016x}", u64::from_be_bytes(nonce)));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        let directory = transport::Directory {
            directory: PrivateIpcDirectory::open(&path).unwrap(),
            path,
        };
        let key = directory.path.join("client-key");
        keygen(&key);
        let authorized = directory.path.join("authorized_keys");
        fs::copy(key.with_extension("pub"), &authorized).unwrap();
        fs::set_permissions(&authorized, fs::Permissions::from_mode(0o600)).unwrap();
        let username = NativeCommand::new("/usr/bin/id")
            .arg("-un")
            .output()
            .unwrap();
        assert!(username.status.success());
        let username = String::from_utf8(username.stdout)
            .unwrap()
            .trim()
            .to_owned();
        let mut fixture = Self {
            children: vec![],
            client_config: directory.path.join("ssh-config"),
            directory,
        };
        let mut hosts = String::new();
        let mut known = String::new();
        for name in ["target", "jump"] {
            let host_key = fixture.directory.path.join(format!("{name}-key"));
            keygen(&host_key);
            let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = reservation.local_addr().unwrap().port();
            drop(reservation);
            let config = fixture.directory.path.join(format!("{name}-sshd"));
            fs::write(&config, format!("ListenAddress 127.0.0.1\nPort {port}\nHostKey {}\nPidFile {}\nAuthorizedKeysFile {}\nStrictModes yes\nUsePAM no\nPasswordAuthentication no\nKbdInteractiveAuthentication no\nPubkeyAuthentication yes\nAllowUsers {username}\nAllowTcpForwarding yes\nAllowStreamLocalForwarding yes\nPermitTTY no\nMaxSessions 0\nLogLevel ERROR\n", host_key.display(), fixture.directory.path.join(format!("{name}-pid")).display(), authorized.display())).unwrap();
            let child = spaceterm_host::process::spawn(
                NativeCommand::new("/usr/sbin/sshd")
                    .args(["-D", "-e", "-f"])
                    .arg(&config)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null()),
            )
            .expect("BLOCKED: isolated sshd could not start");
            fixture.children.push(child);
            tokio::time::timeout(HARNESS_TIMEOUT, async {
                loop {
                    assert!(
                        fixture
                            .children
                            .last_mut()
                            .unwrap()
                            .try_wait()
                            .unwrap()
                            .is_none(),
                        "BLOCKED: isolated sshd refused account fixture"
                    );
                    if tokio::net::TcpStream::connect(("127.0.0.1", port))
                        .await
                        .is_ok()
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("BLOCKED: isolated sshd did not listen");
            let public = fs::read_to_string(host_key.with_extension("pub")).unwrap();
            known.push_str(&format!("{name} {public}"));
            hosts.push_str(&format!("Host {name}\n  HostName 127.0.0.1\n  Port {port}\n  HostKeyAlias {name}\n  User {username}\n"));
            if name == "target" {
                hosts.push_str(&format!("Host target-jump\n  HostName 127.0.0.1\n  Port {port}\n  HostKeyAlias target\n  User {username}\n  ProxyJump jump\n"));
            }
        }
        let known_hosts = fixture.directory.path.join("known_hosts");
        fs::write(&known_hosts, known).unwrap();
        hosts.push_str(&format!("Host *\n  IdentityFile {}\n  IdentitiesOnly yes\n  StrictHostKeyChecking yes\n  UserKnownHostsFile {}\n  GlobalKnownHostsFile /dev/null\n  BatchMode yes\n  ControlMaster no\n  ControlPath none\n", key.display(), known_hosts.display()));
        fs::write(&fixture.client_config, hosts).unwrap();
        fixture
    }
}
impl Drop for OwnedSsh {
    fn drop(&mut self) {
        for child in &mut self.children {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
fn keygen(path: &Path) {
    let status = NativeCommand::new("/usr/bin/ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(
        status.success(),
        "BLOCKED: isolated SSH key generation failed"
    );
}

async fn exercise(route: Route, major: u32, reads: bool) {
    let fixture = Fixture::new(major).await;
    let mut routes = Routes {
        fixture: &fixture,
        route: Route::Ipc,
        wss: None,
        ssh: None,
        next_endpoint: 0,
    };
    assert!(
        !fixture
            .handle
            .snapshot()
            .state()
            .retained
            .as_ref()
            .unwrap()
            .remote_access()
            .value
            .is_enabled()
    );

    if matches!(route, Route::Wss) {
        // WSS is enabled by an explicit paired, durably identified action. No
        // connection, discovery or SSH action is allowed to synthesize it.
        let mut owner = Client::connect(&mut routes, 0, major).await;
        owner.reconcile(&fixture.handle).await;
        assert!(owner.status().await.parts().listeners.is_empty());
        assert_eq!(
            owner
                .command(Command::SetRemoteAccess {
                    server: ServerId::new(fixture.pin.identity),
                    expected_version: fixture
                        .handle
                        .snapshot()
                        .state()
                        .retained
                        .as_ref()
                        .unwrap()
                        .remote_access()
                        .version,
                    value: RemoteAccessPolicy::enabled(
                        ServerId::new(fixture.pin.identity),
                        vec!["127.0.0.1:0".parse().unwrap()]
                    )
                    .unwrap(),
                })
                .await,
            Outcome::Applied
        );
        owner.route.close().await;
        routes.wss = Some(enabled_status(&mut routes, major).await);
    }
    if matches!(route, Route::Ssh | Route::Jump) {
        routes.ssh = Some(OwnedSsh::new().await);
    }
    routes.route = route;
    let before = fixture.handle.snapshot();
    let unpaired = routes.open().await;
    let unpaired_principal = authentication::Principal {
        server: fixture.pin.identity,
        device: [99; 16],
        pairing: native::local(u64::MAX),
        client: native::local(u64::MAX),
    };
    assert!(
        matches!(
            handshake(&unpaired, fixture.pin, unpaired_principal, major, 99).await,
            Err(ProtocolError::Authentication | ProtocolError::PairingDenied)
        ),
        "reachability must not grant paired authority"
    );
    unpaired.close().await;
    assert_eq!(fixture.handle.snapshot(), before);
    let mut a = Client::connect(&mut routes, 0, major).await;
    let generation = a.reconcile(&fixture.handle).await;
    // The explicit WSS enablement consumed operation 1 on this client's stream.
    if matches!(route, Route::Wss) {
        a.sequence = 2;
    }
    let mut b = Client::connect(&mut routes, 1, major).await;
    b.reconcile(&fixture.handle).await;
    let create = a.request(Command::CreateWorkspace {
        server: a.stream.server(),
        source: WorkspaceCreationSource::ServerHome {
            server: a.stream.server(),
        },
        name: NameDraft::Custom(NameText::new("route A".into()).unwrap()),
    });
    let created_a = a.submit(&create).await;
    let Outcome::CreatedWithControl(created_a_value) = created_a.clone() else {
        panic!("created with control: {created_a:?}")
    };
    let created_b = b
        .command(Command::CreateWorkspace {
            server: b.stream.server(),
            source: WorkspaceCreationSource::ServerHome {
                server: b.stream.server(),
            },
            name: NameDraft::Custom(NameText::new("route B".into()).unwrap()),
        })
        .await;
    let Outcome::CreatedWithControl(created_b_value) = created_b else {
        panic!("created with control")
    };
    let grant_a = created_a_value.control().grant();
    let grant_b = created_b_value.control().grant();
    assert_ne!(grant_a.pane(), grant_b.pane());
    let mut acquired_grants = Vec::new();
    for (client, grant) in [(&mut a, grant_a), (&mut b, grant_b)] {
        assert_eq!(
            client
                .command(Command::ReleaseControl(ReleaseControlRequest {
                    epoch: fixture.handle.snapshot().epoch(),
                    grant
                }))
                .await,
            Outcome::Applied
        );
        let snapshot = fixture.handle.snapshot();
        let runtime = snapshot
            .runtime()
            .iter()
            .find(|r| r.pane == grant.pane())
            .unwrap();
        let acquired = client
            .command(Command::CompareAcquire(CompareAcquireRequest {
                epoch: snapshot.epoch(),
                pane: grant.pane(),
                client: client.stream.client(),
                expected_generation: runtime.control_generation,
            }))
            .await;
        let Outcome::ControlGranted(acquired) = acquired else {
            panic!("exact unclaimed comparison acquires control: {acquired:?}")
        };
        acquired_grants.push(acquired.grant());
    }
    let snapshot = fixture.handle.snapshot();
    let runtime_b = snapshot
        .runtime()
        .iter()
        .find(|r| r.pane == grant_b.pane())
        .unwrap();
    let losing = a
        .command(Command::CompareAcquire(CompareAcquireRequest {
            epoch: snapshot.epoch(),
            pane: grant_b.pane(),
            client: a.stream.client(),
            expected_generation: runtime_b.control_generation,
        }))
        .await;
    assert_eq!(
        losing,
        Outcome::Failed(OperationFailure::StaleAuthority),
        "comparison must not take over peer's Pane"
    );
    assert_eq!(
        fixture.handle.snapshot().runtime(),
        snapshot.runtime(),
        "a losing comparison leaves both grants unchanged"
    );
    let grant_a = acquired_grants[0];
    let grant_b = acquired_grants[1];
    let session_a = a.start(&fixture.handle, grant_a).await;
    let session_b = b.start(&fixture.handle, grant_b).await;
    let fence_a = a.fence(session_a, grant_a);
    let fence_b = b.fence(session_b, grant_b);
    tokio::join!(a.subscribe(fence_a), b.subscribe(fence_b));
    tokio::join!(
        a.line(
            fence_a,
            "route_count=$((route_count + 1)); printf 'route-A:%s\\n' \"$route_count\""
        ),
        b.line(fence_b, "printf 'route-B:1\\n'")
    );
    tokio::join!(a.output("route-A:1"), b.output("route-B:1"));
    assert!(
        !a.text.contains("route-B:1"),
        "client A must not receive client B's content"
    );
    assert!(
        !b.text.contains("route-A:1"),
        "client B must not receive client A's content"
    );
    if reads {
        a.authenticated_content_reads().await;
        a.route.close().await;
        b.route.close().await;
        drop(routes);
        fixture.shutdown().await;
        return;
    }
    let original_frame = a.last_frame.clone();
    assert!(
        a.effect(
            a.fence(
                session_b,
                ControlGrant::new(grant_b.pane(), a.stream.client(), grant_b.generation()).unwrap()
            ),
            RuntimeCommand::Key(spaceterm_terminal::KeyInput::text_input("cross-client-key"))
        )
        .await
        .is_some(),
        "peer's fence must not admit input"
    );
    let retained = fixture.handle.snapshot();
    let next_sequence = a.sequence;
    a.route.close().await;
    let mut a = Client::connect(&mut routes, 0, major).await;
    assert_ne!(a.reconcile(&fixture.handle).await, generation);
    a.sequence = next_sequence;
    assert_eq!(fixture.handle.snapshot().state(), retained.state());
    for pane in [grant_a.pane(), grant_b.pane()] {
        let now = fixture.handle.snapshot();
        let current = now
            .runtime()
            .iter()
            .find(|runtime| runtime.pane == pane)
            .unwrap();
        let previous = retained
            .runtime()
            .iter()
            .find(|runtime| runtime.pane == pane)
            .unwrap();
        assert_eq!(
            current.session_generation, previous.session_generation,
            "disconnect preserves Sessions"
        );
        assert_eq!(
            current.control_generation, previous.control_generation,
            "disconnect preserves grants"
        );
        assert_eq!(current.control, previous.control);
    }
    assert_eq!(
        a.submit(&create).await,
        created_a,
        "reconnect retries original identity without creating another Workspace"
    );
    assert_eq!(fixture.handle.snapshot().state(), retained.state());
    a.route.transport.send(&original_frame).await.unwrap();
    assert!(
        tokio::time::timeout(HARNESS_TIMEOUT, a.route.transport.recv())
            .await
            .unwrap()
            .unwrap()
            .is_none(),
        "a signed frame from the previous handshake must close the replacement connection"
    );
    assert_eq!(fixture.handle.snapshot().state(), retained.state());
    a.route.close().await;
    let mut a = Client::connect(&mut routes, 0, major).await;
    a.reconcile(&fixture.handle).await;
    a.sequence = next_sequence;
    a.context(grant_a.pane()).await;
    assert!(
        a.effect(
            fence_a,
            RuntimeCommand::Key(spaceterm_terminal::KeyInput::text_input("stale-input"))
        )
        .await
        .is_some(),
        "a replacement connection cannot reuse the previous input fence"
    );
    let current = a.fence(session_a, grant_a);
    a.subscribe(current).await;
    a.line(current, "printf 'route-A-after:%s\\n' \"$route_count\"")
        .await;
    a.output("route-A-after:1").await;
    assert!(
        !a.text.contains("stale-input"),
        "rejected input is never replayed into the surviving shell"
    );
    assert!(
        !a.text.lines().any(|line| line.trim() == "route-A:2"),
        "accepted input is not replayed after reconnect"
    );
    assert_eq!(
        fixture
            .handle
            .snapshot()
            .runtime()
            .iter()
            .find(|runtime| runtime.pane == grant_a.pane())
            .unwrap()
            .session_generation,
        session_a.generation
    );
    // Every paired route has equal close authority, including closing work
    // controlled by the other client, but only after server assessment.
    for (client, pane) in [(&mut a, grant_b.pane()), (&mut b, grant_a.pane())] {
        let target = CloseTarget::Pane(pane);
        let assessed = client.command(Command::AssessClose { target }).await;
        let Outcome::Assessment(assessment) = assessed else {
            panic!("server close assessment: {assessed:?}")
        };
        assert_eq!(
            client
                .command(Command::CloseConfirmed {
                    target,
                    assessment: assessment.reference()
                })
                .await,
            Outcome::Applied
        );
        assert!(
            fixture
                .handle
                .snapshot()
                .state()
                .panes
                .iter()
                .all(|p| p.id != pane)
        );
    }
    if !matches!(route, Route::Wss) {
        assert!(
            !fixture
                .handle
                .snapshot()
                .state()
                .retained
                .as_ref()
                .unwrap()
                .remote_access()
                .value
                .is_enabled(),
            "IPC and SSH must never provision network listeners"
        );
    }
    if !matches!(route, Route::Wss) {
        assert!(a.status().await.parts().listeners.is_empty());
    }
    a.route.close().await;
    b.route.close().await;
    drop(routes);
    fixture.shutdown().await;
}

macro_rules! route_test {
    ($name:ident, $route:ident, $major:literal) => {
        #[tokio::test]
        async fn $name() {
            exercise(Route::$route, $major, false).await;
        }
    };
}
route_test!(paired_ipc_major_1_has_equivalent_ordinary_authority, Ipc, 1);
route_test!(paired_ipc_major_2_has_equivalent_ordinary_authority, Ipc, 2);
route_test!(paired_wss_major_1_has_equivalent_ordinary_authority, Wss, 1);
route_test!(paired_wss_major_2_has_equivalent_ordinary_authority, Wss, 2);
route_test!(paired_ssh_major_1_has_equivalent_ordinary_authority, Ssh, 1);
route_test!(paired_ssh_major_2_has_equivalent_ordinary_authority, Ssh, 2);
route_test!(
    paired_jump_host_major_1_has_equivalent_ordinary_authority,
    Jump,
    1
);
route_test!(
    paired_jump_host_major_2_has_equivalent_ordinary_authority,
    Jump,
    2
);

#[tokio::test]
async fn authenticated_ipc_major_1_copy_find_topology() {
    exercise(Route::Ipc, 1, true).await;
}
#[tokio::test]
async fn authenticated_ipc_major_2_copy_find_topology() {
    exercise(Route::Ipc, 2, true).await;
}
#[tokio::test]
async fn authenticated_wss_major_1_full_scrollback_copy_find_topology() {
    exercise(Route::Wss, 1, true).await;
}
#[tokio::test]
async fn authenticated_wss_major_2_full_scrollback_copy_find_topology() {
    exercise(Route::Wss, 2, true).await;
}

async fn copy_with_small_negotiated_limits(major: u32) {
    use spaceterm_core::content::CellRef;
    use spaceterm_protocol::limits::NegotiatedLimits;
    for (limits, capacity) in [
        (
            NegotiatedLimits {
                resource_chunk_bytes: 1,
                ..Default::default()
            },
            true,
        ),
        (
            NegotiatedLimits {
                frame_bytes: 4096,
                management_bytes: 4096,
                ..Default::default()
            },
            true,
        ),
        (
            NegotiatedLimits {
                resource_chunk_bytes: 3,
                ..Default::default()
            },
            false,
        ),
    ] {
        let fixture = Fixture::new(major).await;
        let mut routes = Routes {
            fixture: &fixture,
            route: Route::Ipc,
            wss: None,
            ssh: None,
            next_endpoint: 0,
        };
        // Produce content on the ordinary connection. The tiny Copy frame
        // ceiling cannot also carry semantic subscription pages.
        let mut client = Client::connect(&mut routes, 0, major).await;
        client.reconcile(&fixture.handle).await;
        let Outcome::CreatedWithControl(created) = client
            .command(Command::CreateWorkspace {
                server: client.stream.server(),
                source: WorkspaceCreationSource::ServerHome {
                    server: client.stream.server(),
                },
                name: NameDraft::Custom(NameText::new("small Copy limits".into()).unwrap()),
            })
            .await
        else {
            panic!("created workspace")
        };
        let grant = created.control().grant();
        let session = client.start(&fixture.handle, grant).await;
        let fence = client.fence(session, grant);
        client.subscribe(fence).await;
        // Runtime startup precedes the login shell's input readiness.
        tokio::time::timeout(HARNESS_TIMEOUT, async {
            while client.text.trim().is_empty() {
                client.recv().await;
            }
        })
        .await
        .unwrap();
        // Park the shell after output so reconnecting does not race a new prompt.
        client
            .line(
                fence,
                r"printf '\342\202\254x\r\nchunk-ready\r\n'; sleep 60",
            )
            .await;
        client.output("chunk-ready").await;
        let stamp = client.content_stamp.unwrap();
        let row = *client
            .rows
            .iter()
            .find(|(_, text)| text.starts_with('€'))
            .unwrap()
            .0;
        let cell = CellRef {
            session,
            row,
            row_revision: client.row_revisions[&row],
            column: 0,
        };
        let base = stamp.base();
        client.route.close().await;
        let mut client = Client::connect_with_limits(&mut routes, 0, major, limits).await;
        client.reconcile(&fixture.handle).await;
        client.context(grant.pane()).await;
        let fence = client.fence(session, grant);
        let stamp = QueryStamp::new(
            EffectFence::new(
                session,
                grant,
                fence.connection(),
                fence.context(),
                Some(base.revision),
            )
            .unwrap(),
            base,
            ContentRequestId::new(42).unwrap(),
            RequestGeneration::new(1),
        )
        .unwrap();
        let copy = CopyRequest::new(
            stamp,
            ContentSelection::new(stamp, cell, CellRef { column: 1, ..cell }, false).unwrap(),
            CopyOptions {
                unwrap_soft_wraps: true,
                trim_trailing_spaces: true,
                include_html: false,
            },
        )
        .unwrap();
        if capacity {
            client
                .read_query(&content::reads::ReadQuery::Copy(copy))
                .await;
            client.copy_failure(stamp, 4).await;
        } else {
            let (plain, html, chunks) = client.copy_bytes(copy).await;
            assert_eq!(plain, "€x".as_bytes());
            assert!(html.is_empty());
            assert_eq!(chunks, 2);
        }
        // Capacity is a typed read rejection, not a disconnect or truncated success.
        assert_eq!(client.status().await.parts().epoch, session.epoch);
        client.route.close().await;
        drop(routes);
        fixture.shutdown().await;
    }
}

#[tokio::test]
async fn authenticated_ipc_major_1_copy_rejects_insufficient_utf8_and_frame_capacity() {
    copy_with_small_negotiated_limits(1).await;
}
#[tokio::test]
async fn authenticated_ipc_major_2_copy_rejects_insufficient_utf8_and_frame_capacity() {
    copy_with_small_negotiated_limits(2).await;
}

async fn reduced_terminal_pages(major: u32, limits: spaceterm_protocol::limits::NegotiatedLimits) {
    let fixture = Fixture::new(major).await;
    let mut routes = Routes {
        fixture: &fixture,
        route: Route::Ipc,
        wss: None,
        ssh: None,
        next_endpoint: 0,
    };
    let mut client = Client::connect_with_limits(&mut routes, 0, major, limits).await;
    client.reconcile(&fixture.handle).await;
    let Outcome::CreatedWithControl(created) = client
        .command(Command::CreateWorkspace {
            server: client.stream.server(),
            source: WorkspaceCreationSource::ServerHome {
                server: client.stream.server(),
            },
            name: NameDraft::Custom(NameText::new("bounded pages".into()).unwrap()),
        })
        .await
    else {
        panic!("created workspace")
    };
    let grant = created.control().grant();
    let session = client.start(&fixture.handle, grant).await;
    let fence = client.fence(session, grant);
    client
        .subscribe_with_limits(fence, PageLimits::new(256, 16384, 1024 * 1024).unwrap())
        .await;
    let mut pages = 0;
    tokio::time::timeout(HARNESS_TIMEOUT, async {
        loop {
            let payload = client.recv().await;
            let complete = match payload {
                Payload::Major1(v1::Message {
                    kind: Some(v1::message::Kind::ContentPublication(value)),
                }) => value.complete,
                Payload::Major2(v2::Message {
                    kind: Some(v2::message::Kind::ContentPublication(value)),
                }) => value.complete,
                _ => continue,
            };
            pages += 1;
            if complete {
                break;
            }
        }
    })
    .await
    .unwrap();
    assert!(pages > 1);
    assert_eq!(client.rows.len(), 24);
    assert!(client.rows.values().all(|row| row.chars().count() == 100));
    let base = client.base.unwrap();
    let query_fence = EffectFence::new(
        session,
        grant,
        fence.connection(),
        fence.context(),
        Some(base.revision),
    )
    .unwrap();
    let stamp = QueryStamp::new(
        query_fence,
        base,
        ContentRequestId::new(42).unwrap(),
        RequestGeneration::new(1),
    )
    .unwrap();
    let mut continuation = None;
    let mut reconstructed = BTreeMap::<spaceterm_core::content::RowId, String>::new();
    loop {
        let request = ContentPageRequest::new(
            stamp,
            ContentRange::ActiveGrid,
            continuation,
            PageLimits::new(256, 16384, 1024 * 1024).unwrap(),
        )
        .unwrap();
        let payload = if major == 1 {
            Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::ContentQuery(
                    content::v1::encode_page_request(request).unwrap(),
                )),
            })
        } else {
            Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::ContentQuery(
                    content::v2::encode_page_request(request).unwrap(),
                )),
            })
        };
        let result = match client.exchange(payload).await {
            Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::ContentReadResponse(value)),
            }) => match value.result {
                Some(v1::content_read_response::Result::Page(page)) => {
                    ContentReadResult::Ready(content::v1::decode_page(&page, &request).unwrap())
                }
                _ => panic!("ready one-shot grid"),
            },
            Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::ContentReadResponse(value)),
            }) => match value.result {
                Some(v2::content_read_response::Result::Page(page)) => {
                    ContentReadResult::Ready(content::v2::decode_page(&page, &request).unwrap())
                }
                _ => panic!("ready one-shot grid"),
            },
            _ => panic!("one-shot page response"),
        };
        let ContentReadResult::Ready(reply) = result else {
            panic!("stable one-shot grid")
        };
        let parts = reply.page().parts();
        assert!(parts.rows.len() <= limits.semantic_rows);
        assert!(
            parts
                .rows
                .iter()
                .map(|row| row.cells().len())
                .sum::<usize>()
                <= limits.semantic_cells
        );
        for row in &parts.rows {
            reconstructed
                .entry(row.id())
                .or_default()
                .extend(row.cells().iter().map(|cell| cell.content().text()));
        }
        continuation = reply.continuation().unwrap();
        if continuation.is_none() {
            break;
        }
    }
    assert_eq!(reconstructed, client.rows);
    client.status().await;
    client.route.close().await;
    drop(routes);
    fixture.shutdown().await;
}
async fn reduced_terminal_cases(major: u32) {
    use spaceterm_protocol::limits::NegotiatedLimits;
    for limits in [
        NegotiatedLimits {
            frame_bytes: 65536,
            management_bytes: 32768,
            semantic_rows: 4,
            ..Default::default()
        },
        NegotiatedLimits {
            frame_bytes: 32768,
            management_bytes: 32768,
            semantic_cells: 70,
            ..Default::default()
        },
        NegotiatedLimits {
            frame_bytes: 8192,
            management_bytes: 8192,
            ..Default::default()
        },
    ] {
        reduced_terminal_pages(major, limits).await;
    }
}
#[tokio::test]
async fn negotiated_terminal_pages_reconstruct_grid_major_1() {
    reduced_terminal_cases(1).await;
}
#[tokio::test]
async fn negotiated_terminal_pages_reconstruct_grid_major_2() {
    reduced_terminal_cases(2).await;
}

async fn metadata_without_content(major: u32) {
    use spaceterm_terminal::metadata::ProgressMetadata;
    let fixture = Fixture::new(major).await;
    let mut routes = Routes {
        fixture: &fixture,
        route: Route::Ipc,
        wss: None,
        ssh: None,
        next_endpoint: 0,
    };
    let mut client = Client::connect(&mut routes, 0, major).await;
    client.reconcile(&fixture.handle).await;
    let Outcome::CreatedWithControl(created) = client
        .command(Command::CreateWorkspace {
            server: client.stream.server(),
            source: WorkspaceCreationSource::ServerHome {
                server: client.stream.server(),
            },
            name: NameDraft::Custom(NameText::new("metadata stream".into()).unwrap()),
        })
        .await
    else {
        panic!("created workspace")
    };
    let grant = created.control().grant();
    let session = client.start(&fixture.handle, grant).await;
    let fence = client.fence(session, grant);
    client.subscribe(fence).await;
    client
        .line(
            fence,
            r"printf '\033]0;server-metadata-title\007\033]9;4;1;37\007'; sleep 35",
        )
        .await;
    let initial = tokio::time::timeout(HARNESS_TIMEOUT, async {
        loop {
            if let Some(snapshot) = client.metadata.iter().find(|snapshot| {
                snapshot.parts().title.value.as_ref() == "server-metadata-title"
                    && snapshot.parts().progress == ProgressMetadata::Normal(37)
            }) {
                break snapshot.clone();
            }
            client.recv().await;
        }
    })
    .await
    .expect("OSC title and progress must be published");
    let content_at_progress = client.base;
    let expired = tokio::time::timeout(HARNESS_TIMEOUT, async {
        loop {
            if let Some(snapshot) = client.metadata.iter().find(|snapshot| {
                snapshot.parts().sequence > initial.parts().sequence
                    && snapshot.parts().progress == ProgressMetadata::None
                    && snapshot.parts().title.value.as_ref() == "server-metadata-title"
            }) {
                break snapshot.clone();
            }
            client.recv().await;
        }
    })
    .await
    .expect("progress expiry must be published without terminal text");
    assert_eq!(expired.session(), session);
    assert_eq!(
        client.base, content_at_progress,
        "progress expiry needs no content change"
    );
    assert!(expired.parts().observed_at.get() >= initial.parts().observed_at.get() + 30000);
    client.status().await;
    client.route.close().await;
    drop(routes);
    fixture.shutdown().await;
}
#[tokio::test]
async fn runtime_metadata_title_and_progress_expiry_major_1() {
    metadata_without_content(1).await;
}
#[tokio::test]
async fn runtime_metadata_title_and_progress_expiry_major_2() {
    metadata_without_content(2).await;
}

async fn tiny_frame_metadata(major: u32) {
    let fixture = Fixture::new(major).await;
    let mut routes = Routes {
        fixture: &fixture,
        route: Route::Ipc,
        wss: None,
        ssh: None,
        next_endpoint: 0,
    };
    let mut client = Client::connect_with_limits(
        &mut routes,
        0,
        major,
        spaceterm_protocol::limits::NegotiatedLimits {
            frame_bytes: 1024,
            management_bytes: 1024,
            ..Default::default()
        },
    )
    .await;
    client.reconcile(&fixture.handle).await;
    let Outcome::CreatedWithControl(created) = client
        .command(Command::CreateWorkspace {
            server: client.stream.server(),
            source: WorkspaceCreationSource::ServerHome {
                server: client.stream.server(),
            },
            name: NameDraft::Custom(NameText::new("tiny metadata".into()).unwrap()),
        })
        .await
    else {
        panic!("created workspace")
    };
    let grant = created.control().grant();
    let session = client.start(&fixture.handle, grant).await;
    let fence = client.fence(session, grant);
    client.subscribe(fence).await;
    client
        .line(
            fence,
            r#"printf '\033]0;'; i=0; while [ "$i" -lt 256 ]; do printf '\360\220\220\200'; i=$((i+1)); done; printf '\007'"#,
        )
        .await;
    let title = "\u{10400}".repeat(256);
    tokio::time::timeout(Duration::from_secs(10), async {
        while !client.metadata.iter().any(|snapshot| {
            snapshot.session() == session && snapshot.parts().title.value.as_ref() == title
        }) {
            client.recv().await;
        }
    })
    .await
    .expect("a bounded title larger than the frame must arrive intact");
    assert!(client.metadata_fragments >= 2);
    client.status().await;
    client.route.close().await;
    drop(routes);
    fixture.shutdown().await;
}
#[tokio::test]
async fn runtime_metadata_reassembles_tiny_frames_major_1() {
    tiny_frame_metadata(1).await;
}
#[tokio::test]
async fn runtime_metadata_reassembles_tiny_frames_major_2() {
    tiny_frame_metadata(2).await;
}

async fn authenticated_application_mouse(major: u32) {
    use spaceterm_core::content::CellRef;
    use spaceterm_terminal::{
        InputModifiers,
        pointer_input::{PointerButton, PointerPhase},
    };
    let fixture = Fixture::new(major).await;
    let mut routes = Routes {
        fixture: &fixture,
        route: Route::Ipc,
        wss: None,
        ssh: None,
        next_endpoint: 0,
    };
    let mut client = Client::connect(&mut routes, 0, major).await;
    client.reconcile(&fixture.handle).await;
    let Outcome::CreatedWithControl(created) = client
        .command(Command::CreateWorkspace {
            server: client.stream.server(),
            source: WorkspaceCreationSource::ServerHome {
                server: client.stream.server(),
            },
            name: NameDraft::Custom(NameText::new("mouse application".into()).unwrap()),
        })
        .await
    else {
        panic!("created workspace")
    };
    let grant = created.control().grant();
    let session = client.start(&fixture.handle, grant).await;
    let fence = client.fence(session, grant);
    client.subscribe(fence).await;
    let capture = fixture._root.path().join("mouse-input");
    client.line(fence, &format!(r"stty raw -echo; printf '\033[?1002h\033[?1006h\r\nMOUSE_READY\r\n'; dd bs=1 count=38 of='{}' 2>/dev/null; stty -raw echo; printf '\033[?1002l\033[?1006l\r\nMOUSE_DONE\r\n'", capture.display())).await;
    client.output("MOUSE_READY").await;
    assert!(client.mouse_enabled);
    let base = client.base.unwrap();
    let row = *client.rows.keys().next().unwrap();
    let cell = CellRef {
        session,
        row,
        row_revision: client.row_revisions[&row],
        column: 0,
    };
    let captured = EffectFence::new(
        session,
        grant,
        fence.connection(),
        fence.context(),
        Some(base.revision),
    )
    .unwrap();
    let stamp = QueryStamp::new(
        captured,
        base,
        ContentRequestId::new(101).unwrap(),
        RequestGeneration::new(1),
    )
    .unwrap();
    let pointer = |phase| {
        RuntimeCommand::Pointer(ApplicationPointerInput {
            stamp,
            cell,
            phase,
            button: Some(PointerButton::Left),
            modifiers: InputModifiers::default(),
        })
    };
    for phase in [
        PointerPhase::Press,
        PointerPhase::Motion,
        PointerPhase::Release,
    ] {
        assert_eq!(client.effect(captured, pointer(phase)).await, None);
    }
    assert_eq!(
        client
            .effect(
                captured,
                RuntimeCommand::Wheel(ApplicationWheelInput {
                    stamp,
                    cell,
                    horizontal_steps: 0,
                    vertical_steps: 1,
                    modifiers: InputModifiers::default()
                })
            )
            .await,
        None
    );
    client.output("MOUSE_DONE").await;
    assert_eq!(
        fs::read(capture).unwrap(),
        b"\x1b[<0;1;1M\x1b[<32;1;1M\x1b[<0;1;1m\x1b[<64;1;1M"
    );
    assert!(
        client
            .effect(captured, pointer(PointerPhase::Press))
            .await
            .is_some(),
        "content changes reject old mouse coordinates"
    );
    let mut other = Client::connect(&mut routes, 1, major).await;
    other.reconcile(&fixture.handle).await;
    assert!(matches!(
        other
            .command(Command::TakeOver(TakeOverRequest {
                epoch: session.epoch,
                pane: session.pane,
                client: other.stream.client()
            }))
            .await,
        Outcome::ControlGranted(_)
    ));
    assert!(
        client
            .effect(captured, pointer(PointerPhase::Press))
            .await
            .is_some(),
        "takeover rejects the original mouse grant"
    );
    client.status().await;
    client.route.close().await;
    other.route.close().await;
    drop(routes);
    fixture.shutdown().await;
}
#[tokio::test]
async fn authenticated_click_drag_wheel_reject_stale_and_takeover_major_1() {
    authenticated_application_mouse(1).await;
}
#[tokio::test]
async fn authenticated_click_drag_wheel_reject_stale_and_takeover_major_2() {
    authenticated_application_mouse(2).await;
}
