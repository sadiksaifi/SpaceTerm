//! Real account, pairing, listeners, SQLite and native PTYs shared by route acceptance.
#![allow(dead_code)]

pub(crate) use ring::signature::{Ed25519KeyPair, KeyPair};
pub(crate) use spaceterm_core::{
    authority::*, content_queries::*, control::*, identity::*, management::*,
    operation_ledger::canonical::CanonicalCodec, operation_ledger_contracts::*, operations::*,
    organization::*, pairing::*, remote_access::RemoteAccessPolicy,
};
pub(crate) use spaceterm_host::{
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
pub(crate) use spaceterm_protocol::{
    ProtocolError,
    authentication::{self, ClientHandshake, Established, HandshakeConfig, Purpose, SigningKey},
    codec::Payload,
    connection::AuthenticatedConnection,
    convert::{content, identity, management, operations, runtime},
    pairing_adapter::*,
    wire::{v1, v2},
};
pub(crate) use spaceterm_server::{
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
pub(crate) use std::{
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

#[path = "job_a.rs"]
pub(crate) mod native;
#[path = "job_b.rs"]
pub(crate) mod transport;

// Bounds test coordination only, not protocol admission or product deadlines.
pub(crate) const HARNESS_TIMEOUT: Duration = Duration::from_secs(60);

pub(crate) use spaceterm_terminal::{runtime::budget::RuntimeLimits, runtime_contracts::*};

pub(crate) struct Key(Ed25519KeyPair);
impl Key {
    pub(crate) fn new(seed: u8) -> Self {
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
pub(crate) struct Authority(authentication::ServerPin);
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
pub(crate) struct Owner;
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
pub(crate) struct Account(PathBuf, u32);
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
pub(crate) struct Enrollment {
    pub(crate) server: RecoveredServer,
    pub(crate) service: PairingService,
    pub(crate) identity: Arc<spaceterm_host::server_keys::ServerIdentity>,
}
impl Enrollment {
    pub(crate) async fn enroll(
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
pub(crate) struct Fixture {
    pub(crate) _root: tempfile::TempDir,
    pub(crate) ipc: transport::Directory,
    pub(crate) endpoint: IpcEndpoint,
    pub(crate) handle: ServerHandle,
    pub(crate) running:
        tokio::task::JoinHandle<Result<spaceterm_server::shutdown::ShutdownReport, ServerFailure>>,
    pub(crate) stop: Arc<native::Host>,
    pub(crate) streams: [OperationStreamId; 2],
    pub(crate) principals: [authentication::Principal; 2],
    pub(crate) pin: authentication::ServerPin,
}
impl Fixture {
    pub(crate) async fn new(major: u32) -> Self {
        #[cfg(target_os = "macos")]
        let parent = PathBuf::from("/private/tmp");
        #[cfg(target_os = "linux")]
        let parent = PathBuf::from(std::env::var_os("HOME").unwrap());
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
    pub(crate) fn principal(&self, index: usize) -> authentication::Principal {
        self.principals[index]
    }
    pub(crate) async fn shutdown(self) {
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
pub(crate) enum Route {
    Ipc,
    Wss,
    Ssh,
    Jump,
}
pub(crate) struct Routes<'a> {
    pub(crate) fixture: &'a Fixture,
    pub(crate) route: Route,
    pub(crate) wss: Option<SocketAddr>,
    pub(crate) ssh: Option<OwnedSsh>,
    pub(crate) next_endpoint: u64,
}
impl Routes<'_> {
    pub(crate) async fn enable_wss(&mut self, major: u32) {
        let mut owner = Client::connect(self, 0, major).await;
        owner.reconcile(&self.fixture.handle).await;
        assert_eq!(
            owner
                .command(Command::SetRemoteAccess {
                    server: owner.stream.server(),
                    expected_version: self
                        .fixture
                        .handle
                        .snapshot()
                        .state()
                        .retained
                        .as_ref()
                        .unwrap()
                        .remote_access()
                        .version,
                    value: RemoteAccessPolicy::enabled(
                        owner.stream.server(),
                        vec!["127.0.0.1:0".parse().unwrap()]
                    )
                    .unwrap(),
                })
                .await,
            Outcome::Applied
        );
        owner.route.close().await;
        self.wss = Some(enabled_status(self, major).await);
        self.route = Route::Wss;
    }
    pub(crate) async fn open(&mut self) -> OpenRoute {
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
                        HARNESS_TIMEOUT,
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
                    HARNESS_TIMEOUT,
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
pub(crate) struct OpenRoute {
    pub(crate) transport: Arc<dyn MessageTransport>,
    pub(crate) tunnel: Option<SshTunnel>,
}
impl OpenRoute {
    pub(crate) async fn close(self) {
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
pub(crate) struct Client {
    pub(crate) route: OpenRoute,
    pub(crate) codec: AuthenticatedConnection,
    pub(crate) stream: OperationStreamId,
    pub(crate) sequence: u64,
    pub(crate) last_frame: Vec<u8>,
    pub(crate) generation: Option<ConnectionGeneration>,
    pub(crate) runtime_request: u64,
    pub(crate) rows: BTreeMap<spaceterm_core::content::RowId, String>,
    pub(crate) text: String,
}
impl Client {
    pub(crate) async fn connect(routes: &mut Routes<'_>, index: usize, major: u32) -> Self {
        Self::connect_limits(
            routes,
            index,
            major,
            spaceterm_protocol::limits::NegotiatedLimits::default(),
        )
        .await
    }
    pub(crate) async fn connect_limits(
        routes: &mut Routes<'_>,
        index: usize,
        major: u32,
        limits: spaceterm_protocol::limits::NegotiatedLimits,
    ) -> Self {
        let route = routes.open().await;
        let codec = handshake_limits(
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
        }
    }
    pub(crate) fn request(&mut self, command: Command) -> CanonicalOperationRequest {
        let request = CanonicalOperationRequest::new(
            OperationId::new(self.stream, OperationSequence::new(self.sequence).unwrap()),
            command,
            &CanonicalCodec,
        )
        .unwrap();
        self.sequence += 1;
        request
    }
    pub(crate) async fn send(&mut self, payload: Payload) {
        self.last_frame = self.codec.encode(&payload).unwrap();
        self.route.transport.send(&self.last_frame).await.unwrap();
    }
    pub(crate) async fn recv(&mut self) -> Payload {
        let bytes = tokio::time::timeout(HARNESS_TIMEOUT, self.route.transport.recv())
            .await
            .unwrap()
            .unwrap()
            .expect("authenticated response");
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
                    let request = ContentPageRequest::new(
                        stamp,
                        ContentRange::ActiveGrid,
                        None,
                        PageLimits::new(256, 16384, 1024 * 1024).unwrap(),
                    )
                    .unwrap();
                    let decoded = content::$p::decode_page(page, &request).unwrap();
                    for row in &decoded.page().parts().rows {
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
            _ => {}
        }
        payload
    }
    pub(crate) async fn response(&mut self) -> Payload {
        loop {
            let payload = self.recv().await;
            if !matches!(
                &payload,
                Payload::Major1(v1::Message {
                    kind: Some(v1::message::Kind::ContentPublication(_))
                }) | Payload::Major2(v2::Message {
                    kind: Some(v2::message::Kind::ContentPublication(_))
                })
            ) {
                return payload;
            }
        }
    }
    pub(crate) async fn exchange(&mut self, payload: Payload) -> Payload {
        self.send(payload).await;
        self.response().await
    }
    pub(crate) async fn status(&mut self) -> ServerStatus {
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
    pub(crate) async fn context(&mut self, pane: PaneId) {
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
        tokio::time::timeout(HARNESS_TIMEOUT, async {
            loop {
                let (acknowledged, failure) = match self.exchange(request.clone()).await {
                    Payload::Major1(v1::Message {
                        kind: Some(v1::message::Kind::ClientContext(value)),
                    }) => (value.acknowledged, value.failure),
                    Payload::Major2(v2::Message {
                        kind: Some(v2::message::Kind::ClientContext(value)),
                    }) => (value.acknowledged, value.failure),
                    other => panic!("context acknowledgement: {other:?}"),
                };
                if acknowledged {
                    break;
                }
                let failure = failure.expect("typed context refusal");
                assert_eq!(
                    failure.classification,
                    spaceterm_protocol::wire::common::FailureClass::Busy as i32
                );
                tokio::time::sleep(Duration::from_millis(u64::from(
                    failure.retry_after_ms.max(1),
                )))
                .await;
            }
        })
        .await
        .expect("a busy actor eventually admits the exact focus update");
    }
    pub(crate) fn fence(&self, session: SessionKey, grant: ControlGrant) -> EffectFence {
        EffectFence::new(
            session,
            grant,
            ConnectionBinding::new(self.stream.client(), self.generation.unwrap()),
            ContextGeneration::new(2),
            None,
        )
        .unwrap()
    }
    pub(crate) async fn start(&mut self, handle: &ServerHandle, grant: ControlGrant) -> SessionKey {
        self.start_grid(handle, grant, 24, 100).await
    }
    pub(crate) async fn start_grid(
        &mut self,
        handle: &ServerHandle,
        grant: ControlGrant,
        rows: u32,
        columns: u32,
    ) -> SessionKey {
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
            rows,
            columns,
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
    pub(crate) async fn subscribe(&mut self, fence: EffectFence) {
        self.subscribe_limits(fence, PageLimits::new(4, 1024, 16384).unwrap())
            .await;
    }
    pub(crate) async fn subscribe_limits(&mut self, fence: EffectFence, limits: PageLimits) {
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
    pub(crate) async fn effect(
        &mut self,
        fence: EffectFence,
        command: RuntimeCommand,
    ) -> Option<i32> {
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
    pub(crate) async fn line(&mut self, fence: EffectFence, text: &str) {
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
    pub(crate) async fn output(&mut self, expected: &str) {
        tokio::time::timeout(HARNESS_TIMEOUT, async {
            while !self.text.lines().any(|line| line.trim() == expected) {
                self.recv().await;
            }
        })
        .await
        .expect("semantic subscription reaches expected shell output");
    }
    pub(crate) async fn submit(&mut self, request: &CanonicalOperationRequest) -> Outcome {
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
            let original_payload = payload.clone();
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
                    OperationLookup::NeverAdmitted => {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                        payload = original_payload.clone();
                        continue;
                    }
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

    pub(crate) async fn command(&mut self, command: Command) -> Outcome {
        let request = self.request(command);
        self.submit(&request).await
    }
    pub(crate) async fn reconcile(&mut self, handle: &ServerHandle) -> ConnectionGeneration {
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
pub(crate) async fn handshake(
    route: &OpenRoute,
    pin: authentication::ServerPin,
    principal: authentication::Principal,
    major: u32,
    seed: u8,
) -> Result<AuthenticatedConnection, ProtocolError> {
    handshake_limits(
        route,
        pin,
        principal,
        major,
        seed,
        spaceterm_protocol::limits::NegotiatedLimits::default(),
    )
    .await
}
async fn handshake_limits(
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
            capabilities: vec![
                spaceterm_protocol::negotiation::CAPABILITY_CANONICAL_DOMAIN,
                spaceterm_protocol::negotiation::CAPABILITY_TERMINAL_STREAMS,
            ],
            limits,
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

pub(crate) async fn enabled_status(routes: &mut Routes<'_>, major: u32) -> SocketAddr {
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
pub(crate) struct OwnedSsh {
    pub(crate) children: Vec<Child>,
    pub(crate) client_config: PathBuf,
    pub(crate) directory: transport::Directory,
}
impl OwnedSsh {
    pub(crate) async fn new() -> Self {
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
pub(crate) fn keygen(path: &Path) {
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
