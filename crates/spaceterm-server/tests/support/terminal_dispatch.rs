//! Real authenticated IPC, SQLite, actor and PTY vertical. No production test adapter.
use super::*;
use spaceterm_core::{content_queries::*, control::*, operations::*, organization::*};
use spaceterm_host::transport::MessageTransport;
use spaceterm_protocol::{
    authentication::{self, Established, HandshakeConfig, Purpose},
    codec::Payload,
    convert::{content, identity, operations, runtime},
    wire::{v1, v2},
};
use spaceterm_server::{
    adapters::{
        core::CorePorts,
        terminal::{AccountLaunchFacts, AccountLaunchProvider, NativeRuntime, NativeRuntimeClock},
    },
    api::*,
    connection::{AuthenticationConfig, Authenticator, PeerConnection},
    contracts::*,
    listeners::ConnectionDispatchPort,
    protocol_dispatch::{ActorOrdinaryDispatch, ProtocolDispatcher},
    service::RecoveredServer,
};
use spaceterm_terminal::{runtime::budget::RuntimeLimits, runtime_contracts::*};
use std::{
    collections::{BTreeMap, VecDeque},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

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
struct Peer {
    transport: Arc<spaceterm_host::ipc::IpcTransport>,
    codec: spaceterm_protocol::connection::AuthenticatedConnection,
    principal: VerifiedPrincipal,
    stream: OperationStreamId,
    running: tokio::task::JoinHandle<Result<(), ServerFailure>>,
    stage: BTreeMap<spaceterm_core::content::RowId, String>,
    text: String,
    transfer: u64,
    next_index: u32,
    parts_seen: u32,
    outcomes: BTreeMap<u64, Option<i32>>,
    resource: Option<(QueryStamp, spaceterm_core::content::GraphicsResourceRef)>,
    pages: Vec<spaceterm_core::content::ContentPage>,
    page_complete: bool,
    native_seen: usize,
    native_pending: VecDeque<spaceterm_core::native_requests::NativeRequest>,
}
impl Peer {
    async fn send(&mut self, payload: Payload) {
        self.transport
            .send(&self.codec.encode(&payload).unwrap())
            .await
            .unwrap();
    }
    async fn recv(&mut self) -> Payload {
        let bytes = tokio::time::timeout(Duration::from_secs(15), self.transport.recv())
            .await
            .unwrap()
            .unwrap()
            .expect("authenticated peer remains open");
        let payload = self.codec.decode(&bytes).unwrap();
        macro_rules! record {
            ($publication:expr, $p:ident) => {{
                let publication = $publication;
                if let Some($p::ContentReadResponse {
                    result: Some($p::content_read_response::Result::Page(page)),
                }) = &publication.response
                {
                    if publication.index == 0 {
                        self.stage.clear();
                        self.pages.clear();
                        self.page_complete = false;
                        self.transfer = publication.transfer;
                        self.next_index = 0;
                    }
                    assert_eq!(publication.transfer, self.transfer);
                    assert_eq!(publication.index, self.next_index);
                    self.next_index += 1;
                    self.parts_seen += 1;
                    let stamp = content::$p::decode_stamp(page.stamp.as_ref().unwrap()).unwrap();
                    let request = ContentPageRequest::new(
                        stamp,
                        ContentRange::ActiveGrid,
                        None,
                        PageLimits::new(256, 16384, 1024 * 1024).unwrap(),
                    )
                    .unwrap();
                    let decoded = content::$p::decode_page(page, &request).unwrap();
                    if let Some(resource) = decoded.page().parts().resources.first() {
                        self.resource = Some((stamp, *resource));
                    }
                    assert!(decoded.page().parts().rows.len() <= 4);
                    assert!(
                        decoded
                            .page()
                            .parts()
                            .rows
                            .iter()
                            .map(|row| row.cells().len())
                            .sum::<usize>()
                            <= 1024
                    );
                    for row in &decoded.page().parts().rows {
                        let text: String = row
                            .cells()
                            .iter()
                            .map(|cell| cell.content().text())
                            .collect();
                        if row.start_column() == 0 {
                            assert!(self.stage.insert(row.id(), text).is_none());
                        } else {
                            self.stage
                                .get_mut(&row.id())
                                .expect("fragment prefix must already be staged")
                                .push_str(&text);
                        }
                    }
                    self.pages.push(decoded.page().clone());
                    self.page_complete = publication.complete;
                    if publication.complete {
                        self.text = self.stage.values().cloned().collect::<Vec<_>>().join("\n");
                    }
                } else {
                    self.stage.clear();
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
                kind: Some(v1::message::Kind::RuntimeOutcome(value)),
            }) => {
                self.outcomes.insert(
                    value.request,
                    value.failure.as_ref().map(|failure| failure.classification),
                );
            }
            Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::RuntimeOutcome(value)),
            }) => {
                self.outcomes.insert(
                    value.request,
                    value.failure.as_ref().map(|failure| failure.classification),
                );
            }
            Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::NativeRequest(value)),
            }) => {
                self.native_seen += 1;
                self.native_pending.push_back(
                    spaceterm_protocol::convert::native_requests::v1::decode_request(
                        value,
                        spaceterm_core::observations::MonotonicTick::new(0),
                    )
                    .unwrap(),
                );
            }
            Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::NativeRequest(value)),
            }) => {
                self.native_seen += 1;
                self.native_pending.push_back(
                    spaceterm_protocol::convert::native_requests::v2::decode_request(
                        value,
                        spaceterm_core::observations::MonotonicTick::new(0),
                    )
                    .unwrap(),
                );
            }
            _ => {}
        }
        payload
    }
    async fn reconcile(&mut self, revision: OrganizationRevision) {
        macro_rules! payload {
            ($p:ident, $major:ident) => {
                Payload::$major($p::Message {
                    kind: Some($p::message::Kind::Reconciliation($p::Reconciliation {
                        request: 1,
                        request_generation: 1,
                        server: Some(identity::encode_global(self.principal.server().bytes())),
                        epoch: self.principal.epoch().get(),
                        client: Some(identity::encode_client(self.principal.client())),
                        organization_revision: revision.get(),
                        ..Default::default()
                    })),
                })
            };
        }
        self.send(match self.codec.major() {
            1 => payload!(v1, Major1),
            _ => payload!(v2, Major2),
        })
        .await;
        loop {
            match self.recv().await {
                Payload::Major1(v1::Message {
                    kind: Some(v1::message::Kind::Reconciliation(value)),
                }) => {
                    assert!(value.acknowledged);
                    assert_eq!(
                        value.connection_generation,
                        self.principal.binding().generation.get()
                    );
                    break;
                }
                Payload::Major2(v2::Message {
                    kind: Some(v2::message::Kind::Reconciliation(value)),
                }) => {
                    assert!(value.acknowledged);
                    assert_eq!(
                        value.connection_generation,
                        self.principal.binding().generation.get()
                    );
                    break;
                }
                _ => {}
            }
        }
    }
    async fn operation(&mut self, sequence: u64, command: Command) -> Outcome {
        let request = spaceterm_core::operation_ledger_contracts::CanonicalOperationRequest::new(
            OperationId::new(self.stream, OperationSequence::new(sequence).unwrap()),
            command,
            &spaceterm_core::operation_ledger::canonical::CanonicalCodec,
        )
        .unwrap();
        self.send(match self.codec.major() {
            1 => Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::Operation(
                    operations::v1::encode_request(&request).unwrap(),
                )),
            }),
            _ => Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::Operation(
                    operations::v2::encode_request(&request).unwrap(),
                )),
            }),
        })
        .await;
        loop {
            match self.recv().await {
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
                }) => {
                    match operations::v1::decode_lookup_result(&value, request.binding()).unwrap() {
                        OperationLookup::Completed { result, .. } => return result,
                        OperationLookup::Pending => {
                            tokio::time::sleep(Duration::from_millis(10)).await;
                            self.send(Payload::Major1(v1::Message {
                                kind: Some(v1::message::Kind::OperationLookupRequest(
                                    operations::v1::encode_lookup_request(request.binding()),
                                )),
                            }))
                            .await;
                        }
                        _ => panic!("identified operation must complete"),
                    }
                }
                Payload::Major2(v2::Message {
                    kind: Some(v2::message::Kind::OperationLookupResult(value)),
                }) => {
                    match operations::v2::decode_lookup_result(&value, request.binding()).unwrap() {
                        OperationLookup::Completed { result, .. } => return result,
                        OperationLookup::Pending => {
                            tokio::time::sleep(Duration::from_millis(10)).await;
                            self.send(Payload::Major2(v2::Message {
                                kind: Some(v2::message::Kind::OperationLookupRequest(
                                    operations::v2::encode_lookup_request(request.binding()),
                                )),
                            }))
                            .await;
                        }
                        _ => panic!("identified operation must complete"),
                    }
                }
                _ => {}
            }
        }
    }
    async fn context(&mut self, pane: PaneId, context: u64, input_focus: bool) {
        macro_rules! payload {
            ($p:ident, $major:ident) => {
                Payload::$major($p::Message {
                    kind: Some($p::message::Kind::ClientContext($p::ClientContext {
                        request: 2,
                        epoch: self.principal.epoch().get(),
                        connection_generation: self.principal.binding().generation.get(),
                        context_generation: context,
                        focused_pane: Some(identity::encode_pane(pane)),
                        terminal_input_focus: input_focus,
                        native_effects_eligible: input_focus,
                        acknowledged: false,
                        failure: None,
                    })),
                })
            };
        }
        self.send(match self.codec.major() {
            1 => payload!(v1, Major1),
            _ => payload!(v2, Major2),
        })
        .await;
        loop {
            match self.recv().await {
                Payload::Major1(v1::Message {
                    kind: Some(v1::message::Kind::ClientContext(value)),
                }) => {
                    assert!(value.acknowledged);
                    break;
                }
                Payload::Major2(v2::Message {
                    kind: Some(v2::message::Kind::ClientContext(value)),
                }) => {
                    assert!(value.acknowledged);
                    break;
                }
                _ => {}
            }
        }
    }
    async fn effect(
        &mut self,
        request: u64,
        fence: EffectFence,
        command: RuntimeCommand,
    ) -> Option<i32> {
        let command = FencedRuntimeCommand { fence, command };
        self.send(match self.codec.major() {
            1 => Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::RuntimeCommand(
                    runtime::v1::encode_command(request, &command).unwrap(),
                )),
            }),
            _ => Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::RuntimeCommand(
                    runtime::v2::encode_command(request, &command).unwrap(),
                )),
            }),
        })
        .await;
        loop {
            if let Some(result) = self.outcomes.remove(&request) {
                return result;
            }
            self.recv().await;
        }
    }

    async fn line(&mut self, request: u64, fence: EffectFence, text: &str) {
        assert_eq!(
            self.effect(
                request,
                fence,
                RuntimeCommand::Key(spaceterm_terminal::KeyInput::text_input(text))
            )
            .await,
            None
        );
        let mut enter = spaceterm_terminal::KeyInput::text_input("");
        enter.physical_key = spaceterm_terminal::PhysicalKey::Enter;
        enter.logical_key = "enter".into();
        enter.text = None;
        assert_eq!(
            self.effect(request + 1, fence, RuntimeCommand::Key(enter))
                .await,
            None
        );
    }
    async fn subscribe(&mut self, fence: EffectFence, generation: u64, cancel: bool) {
        macro_rules! payload {
            ($p:ident, $major:ident) => {
                Payload::$major($p::Message {
                    kind: Some($p::message::Kind::PaneSubscription($p::PaneSubscription {
                        request: 9,
                        request_generation: generation,
                        fence: Some(identity::encode_fence(fence)),
                        cancel,
                        limits: (!cancel).then_some($p::PageLimits {
                            rows: 4,
                            cells: 1024,
                            bytes: 16 * 1024,
                        }),
                    })),
                })
            };
        }
        self.send(match self.codec.major() {
            1 => payload!(v1, Major1),
            _ => payload!(v2, Major2),
        })
        .await;
    }
    async fn native_request(&mut self) -> spaceterm_core::native_requests::NativeRequest {
        loop {
            // Native work may precede the runtime input acknowledgement. A
            // helper waiting for another reply must not discard that work.
            if let Some(request) = self.native_pending.pop_front() {
                return request;
            }
            self.recv().await;
        }
    }
    async fn complete_native(
        &mut self,
        request: &spaceterm_core::native_requests::NativeRequest,
        kind: spaceterm_core::native_requests::NativeCompletionKind,
    ) -> Option<i32> {
        let completion =
            spaceterm_core::native_requests::NativeCompletion::new(request, kind).unwrap();
        self.outcomes.remove(&request.id().get());
        self.send(match self.codec.major() {
            1 => Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::NativeCompletion(
                    spaceterm_protocol::convert::native_requests::v1::encode_completion(
                        &completion,
                    )
                    .unwrap(),
                )),
            }),
            _ => Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::NativeCompletion(
                    spaceterm_protocol::convert::native_requests::v2::encode_completion(
                        &completion,
                    )
                    .unwrap(),
                )),
            }),
        })
        .await;
        loop {
            if let Some(result) = self.outcomes.remove(&request.id().get()) {
                return result;
            }
            self.recv().await;
        }
    }
    async fn output(&mut self, expected: &str) {
        tokio::time::timeout(Duration::from_secs(15), async {
            while !self.text.lines().any(|line| line.trim() == expected) {
                self.recv().await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("semantic subscription reaches fixture marker {expected}"));
    }
    async fn close(self) {
        self.transport.close().await.unwrap();
        self.running.await.unwrap().unwrap();
    }
}
async fn peer(
    ipc: &authentication_fixture::Directory,
    listener: &spaceterm_host::ipc::IpcListener,
    authenticator: &Authenticator,
    dispatcher: Arc<ProtocolDispatcher>,
    handle: ServerHandle,
    handshake: (Arc<Key>, HandshakeConfig),
    identity: (OperationStreamId, u8),
) -> Peer {
    let (keys, config) = handshake;
    let (stream, device) = identity;
    let transport = Arc::new(ipc.connect().await);
    let pending = listener.accept().await.unwrap();
    let claimed = authentication::Principal {
        server: physical::identity().server.bytes(),
        device: [device; 16],
        pairing: actor_fixture::local(stream.pairing().local()),
        client: actor_fixture::local(stream.client().local()),
    };
    let handshake = async {
        let (handshake, hello) = authentication::ClientHandshake::begin(
            keys.pin(),
            claimed,
            Purpose::Ordinary,
            config,
            Arc::new(Key::new(device + 1)),
        )
        .unwrap();
        transport.send(&hello).await.unwrap();
        let (handshake, proof) = handshake
            .receive_server(&transport.recv().await.unwrap().unwrap())
            .unwrap();
        transport.send(&proof).await.unwrap();
        let Established::Ordinary(codec) = handshake
            .receive_ready(&transport.recv().await.unwrap().unwrap())
            .unwrap()
        else {
            panic!("ordinary")
        };
        codec
    };
    let (peer, codec) = tokio::join!(
        authenticator.authenticate(pending, authenticator.try_admit().unwrap()),
        handshake
    );
    let mut peer = peer.unwrap();
    let PeerConnection::Ordinary(session) = &peer.connection else {
        panic!("ordinary")
    };
    let principal = session.principal;
    let running = tokio::spawn(async move {
        let result = dispatcher.run(&mut peer, handle).await;
        peer.close().await.unwrap();
        result
    });
    Peer {
        transport,
        codec,
        principal,
        stream,
        running,
        stage: BTreeMap::new(),
        text: String::new(),
        transfer: 0,
        next_index: 0,
        parts_seen: 0,
        outcomes: BTreeMap::new(),
        resource: None,
        pages: vec![],
        page_complete: false,
        native_seen: 0,
        native_pending: VecDeque::new(),
    }
}
fn client_replica(
    snapshot: &OrganizationSnapshot,
) -> Arc<spaceterm_client::replica::ReplicaSnapshot> {
    let mut replica = spaceterm_client::replica::Replica::new(
        snapshot.state().server,
        Arc::new(
            spaceterm_client::replica::OrganizationResidency::new(
                spaceterm_client::limits::ClientLimits::default(),
            )
            .unwrap(),
        ),
    );
    let token = replica.begin_resync(snapshot.epoch()).unwrap();
    replica.install_snapshot(token, snapshot.clone()).unwrap();
    replica.subscribe().borrow().retained().cloned().unwrap()
}
fn client_info(
    principal: VerifiedPrincipal,
    major: u32,
    revision: OrganizationRevision,
) -> spaceterm_client::api::ConnectedInfo {
    spaceterm_client::api::ConnectedInfo {
        server: principal.server(),
        pairing: principal.pairing(),
        client: principal.client(),
        epoch: principal.epoch(),
        binding: principal.binding(),
        major,
        limits: Default::default(),
        capabilities: spaceterm_client::api::ConnectionCapabilities::NONE,
        organization_revision: revision,
        attempt: 1,
    }
}
fn fence(
    principal: VerifiedPrincipal,
    session: SessionKey,
    grant: GrantedControl,
    context: u64,
) -> EffectFence {
    EffectFence::new(
        session,
        grant.grant(),
        principal.binding(),
        ContextGeneration::new(context),
        None,
    )
    .unwrap()
}

struct StartedTerminal {
    directory: physical::Directory,
    stream2: OperationStreamId,
    keys: Arc<Key>,
    ipc: authentication_fixture::Directory,
    listener: spaceterm_host::ipc::IpcListener,
    host: Arc<actor_fixture::Host>,
    server: RecoveredServer,
    handle: ServerHandle,
    handshake: HandshakeConfig,
    authenticator: Authenticator,
    dispatcher: Arc<ProtocolDispatcher>,
    first: Peer,
    created: CreatedOrganization,
    pane: PaneId,
    session: SessionKey,
}

async fn started_terminal(major: u32) -> StartedTerminal {
    started_terminal_with_limits(major, Default::default()).await
}

impl spaceterm_core::observations::RuntimeClock for actor_fixture::Clock {
    fn now(
        &self,
    ) -> Result<
        spaceterm_core::observations::MonotonicTick,
        spaceterm_core::observations::ObservationError,
    > {
        ServerClock::now(self)
            .map_err(|_| spaceterm_core::observations::ObservationError::ZeroCounter)
    }
}

// PTY scheduling is real, but native-effect deadline assertions own their time.
struct ManualClock(std::sync::atomic::AtomicU64);
impl ManualClock {
    fn new() -> Self {
        Self(std::sync::atomic::AtomicU64::new(1))
    }
    fn advance(&self, millis: u64) {
        self.0
            .fetch_add(millis, std::sync::atomic::Ordering::SeqCst);
    }
}
impl ServerClock for ManualClock {
    fn now(&self) -> Result<spaceterm_core::observations::MonotonicTick, ServerFailure> {
        Ok(spaceterm_core::observations::MonotonicTick::new(
            self.0.load(std::sync::atomic::Ordering::SeqCst),
        ))
    }
}
impl spaceterm_core::observations::RuntimeClock for ManualClock {
    fn now(
        &self,
    ) -> Result<
        spaceterm_core::observations::MonotonicTick,
        spaceterm_core::observations::ObservationError,
    > {
        ServerClock::now(self)
            .map_err(|_| spaceterm_core::observations::ObservationError::ZeroCounter)
    }
}

async fn started_terminal_with_limits(
    major: u32,
    limits: spaceterm_protocol::limits::NegotiatedLimits,
) -> StartedTerminal {
    started_terminal_with_clock(major, limits, Arc::new(NativeRuntimeClock::default())).await
}

async fn started_terminal_with_clock<C>(
    major: u32,
    limits: spaceterm_protocol::limits::NegotiatedLimits,
    clock: Arc<C>,
) -> StartedTerminal
where
    C: ServerClock + spaceterm_core::observations::RuntimeClock + 'static,
{
    let directory = physical::Directory::new();
    let mut storage = physical::Fixture::new(&directory.0);
    storage.enroll(42);
    storage.enroll(44);
    let recovered = storage.recovered();
    let stream1 = recovered.pairing.as_ref().unwrap().parts().authorizations[0]
        .parts()
        .clients[0]
        .parts()
        .ordinary;
    let stream2 = recovered.pairing.as_ref().unwrap().parts().authorizations[1]
        .parts()
        .clients[0]
        .parts()
        .ordinary;
    let snapshot = storage.snapshot();
    let pairings = Arc::new(PhysicalPairings(snapshot));
    let keys = Arc::new(Key::new(71));
    let ipc = authentication_fixture::Directory::new();
    let listener = ipc.listener();
    let probe = ipc.connect().await;
    let uid = probe.peer_facts().local_uid.unwrap();
    let pending_probe = listener.accept().await.unwrap();
    probe.close().await.unwrap();
    drop(pending_probe);
    let native = NativeRuntime::new(
        physical::identity().server,
        uid,
        Arc::new(Account(directory.0.clone(), uid)),
        clock.clone(),
        RuntimeLimits::default(),
    )
    .unwrap();
    let mut config = actor_fixture::Fixture::new();
    let host = config.host.clone();
    config.config.host.authority = Arc::new(PhysicalStartup);
    config.config.core = CorePorts::production(
        physical::identity(),
        ServerDirectory::new(
            physical::identity().server,
            directory.0.as_os_str().as_encoded_bytes().to_vec(),
        )
        .unwrap(),
        clock.clone(),
    )
    .unwrap();
    config.config.launch = native.clone();
    config.config.cleanup = native.clone();
    config.dependencies.runtime = native;
    config.dependencies.clock = clock.clone();
    config.dependencies.store = Box::new(storage.store);
    config.dependencies.credentials = keys.clone();
    config.dependencies.pairing = pairings.clone();
    let server = RecoveredServer::recover(config.config, config.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    let handshake = HandshakeConfig {
        majors: vec![major],
        limits: spaceterm_protocol::limits::NegotiatedLimits {
            resource_chunk_bytes: if major == 1 {
                limits.resource_chunk_bytes
            } else {
                limits.resource_chunk_bytes.min(64 * 1024)
            },
            ..limits
        },
        capabilities: vec![
            spaceterm_protocol::negotiation::CAPABILITY_CANONICAL_DOMAIN,
            spaceterm_protocol::negotiation::CAPABILITY_TERMINAL_STREAMS,
            spaceterm_protocol::negotiation::CAPABILITY_NATIVE_NOTIFICATIONS,
        ],
    };
    let authenticator = Authenticator::new(AuthenticationConfig {
        server: physical::identity().server,
        epoch: handle.snapshot().epoch(),
        owning_uid: uid,
        handshake: handshake.clone(),
        credentials: keys.clone(),
        pairing: pairings,
        authority: Arc::new(handle.clone()),
        unauthenticated_limit: 16,
        authenticated_limit: 32,
        deadline: Duration::from_secs(5),
    })
    .unwrap();
    let (_, status) = tokio::sync::watch::channel(authentication_fixture::status(1));
    let dispatcher = Arc::new(
        ProtocolDispatcher::new(
            authenticator.clone(),
            status,
            Arc::new(ActorOrdinaryDispatch::new(Arc::new(
                spaceterm_core::operation_ledger::canonical::CanonicalCodec,
            ))),
            Arc::new(RejectRestricted),
        )
        .with_clock(clock),
    );
    let mut first = peer(
        &ipc,
        &listener,
        &authenticator,
        dispatcher.clone(),
        handle.clone(),
        (keys.clone(), handshake.clone()),
        (stream1, 42),
    )
    .await;
    first.reconcile(handle.snapshot().state().revision).await;
    let Outcome::CreatedWithControl(created) = first
        .operation(
            1,
            Command::CreateWorkspace {
                server: physical::identity().server,
                source: WorkspaceCreationSource::ServerHome {
                    server: physical::identity().server,
                },
                name: NameDraft::Custom(NameText::new("terminal vertical".into()).unwrap()),
            },
        )
        .await
    else {
        panic!("created with control")
    };
    let pane = created.entities().pane();
    first.context(pane, 2, true).await;
    let pre_session = SessionKey {
        pane,
        epoch: first.principal.epoch(),
        generation: handle.snapshot().runtime()[0].session_generation,
    };
    let startup_fence = fence(first.principal, pre_session, created.control(), 2);
    let intent = StartupIntent::new(
        OperationId::new(stream1, OperationSequence::new(2).unwrap()),
        startup_fence,
        StartupSource::ExplicitSelection,
    )
    .unwrap();
    assert!(matches!(
        first.operation(2, Command::BeginStartup(intent)).await,
        Outcome::StartupAwaitingMeasurement { .. }
    ));
    let grid = spaceterm_core::geometry::MeasuredGrid::new(
        37,
        113,
        spaceterm_core::geometry::CellPixels::new(9, 18).unwrap(),
        spaceterm_core::geometry::SUPPORTED_GRID_LIMITS,
    )
    .unwrap();
    let Outcome::SessionStarted(session) = first
        .operation(
            3,
            Command::CompleteMeasuredStartup(MeasuredStartupMeasurement {
                intent: intent.id(),
                fence: startup_fence,
                grid,
            }),
        )
        .await
    else {
        panic!("session started")
    };
    let mut snapshots = handle.subscribe_snapshot();
    while !snapshots.borrow().runtime().iter().any(|runtime| matches!(runtime.phase, SessionPhase::Running { generation } if generation == session.generation)) { snapshots.changed().await.unwrap(); }
    StartedTerminal {
        directory,
        stream2,
        keys,
        ipc,
        listener,
        host,
        server,
        handle,
        handshake,
        authenticator,
        dispatcher,
        first,
        created,
        pane,
        session,
    }
}

#[tokio::test]
async fn authenticated_ipc_terminal_input_pages_and_two_client_takeover_both_majors() {
    for major in [1, 2] {
        let clock = Arc::new(ManualClock::new());
        let StartedTerminal {
            directory,
            stream2,
            keys,
            ipc,
            listener,
            host,
            server,
            handle,
            handshake,
            authenticator,
            dispatcher,
            mut first,
            created,
            pane,
            session,
        } = started_terminal_with_clock(major, Default::default(), Arc::clone(&clock)).await;
        // The provider is inert in this read-only projection. All content and
        // authority facts below come from the real authenticated server.
        let fake = spaceterm_client::testing::FakeServer::new(11, vec![major]).unwrap();
        let provider = spaceterm_client::testing::FakePaneConnection::new(fake, 1, major)
            .await
            .unwrap();
        let channels = spaceterm_client::pane_channel::PaneChannels::new(
            provider,
            spaceterm_client::projection::ProjectionBudgets::supported().unwrap(),
        )
        .unwrap();
        let snapshot = handle.snapshot();
        channels
            .publish(
                client_info(first.principal, major, snapshot.state().revision),
                client_replica(&snapshot),
            )
            .unwrap();
        let lease = channels
            .attach(
                snapshot.state().workspaces[0].id,
                spaceterm_client::view::AttachmentIncarnation::new(1).unwrap(),
            )
            .unwrap();
        let projection_pane = lease.pane(pane).unwrap();
        let projection_context = projection_pane
            .update_context(spaceterm_client::pane_channel::PaneContextFacts {
                presentable: true,
                focused_pane: true,
                terminal_input_focus: true,
            })
            .unwrap();
        first.context(pane, projection_context.get(), true).await;
        let mut original = fence(
            first.principal,
            session,
            created.control(),
            projection_context.get(),
        );
        first.subscribe(original, 1, false).await;
        first.line(1, original, "PS1=; echo spaceterm-ok").await;
        first.output("spaceterm-ok").await;
        assert!(
            first.parts_seen > 1,
            "bounded semantic transfer must be multipart"
        );
        // A real Kitty resource crosses authenticated IPC in bounded RGBA chunks.
        first.line(3, original, r"printf '\033_Ga=T,f=32,s=300,v=300,i=7,q=2;'; head -c 360000 /dev/zero | base64 | tr -d '\n'; printf '\033\\\n' ").await;
        while first.resource.is_none() || !first.page_complete {
            first.recv().await;
        }
        let (stamp, resource) = first.resource.unwrap();
        let request =
            GraphicsRequest::new(stamp, ResourceTransferId::new(7).unwrap(), resource, 0).unwrap();
        macro_rules! resource_payload {
            ($p:ident, $major:ident) => {
                Payload::$major($p::Message {
                    kind: Some($p::message::Kind::ResourceRequest($p::ResourceRequest {
                        fence: Some(identity::encode_fence(request.stamp().fence())),
                        resource: Some($p::ResourceId {
                            session: Some(identity::encode_session(resource.key.session)),
                            local_id: resource.key.id.get(),
                            generation: resource.key.generation,
                        }),
                        transfer: 7,
                        offset: 0,
                        stamp: Some(Box::new(content::$p::encode_stamp(stamp))),
                        width: resource.width,
                        height: resource.height,
                        rgba_bytes: resource.rgba_bytes,
                        digest: resource.digest.to_vec(),
                    })),
                })
            };
        }
        first
            .send(match major {
                1 => resource_payload!(v1, Major1),
                _ => resource_payload!(v2, Major2),
            })
            .await;
        projection_pane
            .publish_content_base(original, stamp.base())
            .unwrap();
        let projection = projection_pane.projection();
        for page in &first.pages {
            projection
                .lock()
                .unwrap()
                .accept_page(stamp, page.clone())
                .unwrap();
        }
        let mut preparation = projection.lock().unwrap().begin_graphics(request).unwrap();
        let mut asset = None;
        let mut rgba = Vec::new();
        let mut chunks = 0;
        loop {
            macro_rules! accept {
                ($chunk:expr, $p:ident) => {{
                    let chunk = $chunk;
                    assert_eq!(chunk.transfer, 7);
                    assert_eq!(chunk.offset as usize, rgba.len());
                    assert!(chunk.data.len() <= first.codec.limits().resource_chunk_bytes);
                    let decoded = content::$p::decode_graphics_chunk(&chunk, request).unwrap();
                    asset = preparation.accept(decoded).unwrap();
                    rgba.extend_from_slice(&chunk.data);
                    chunks += 1;
                    chunk.complete
                }};
            }
            let complete = match first.recv().await {
                Payload::Major1(v1::Message {
                    kind: Some(v1::message::Kind::ResourceChunk(chunk)),
                }) => accept!(chunk, v1),
                Payload::Major2(v2::Message {
                    kind: Some(v2::message::Kind::ResourceChunk(chunk)),
                }) => accept!(chunk, v2),
                _ => false,
            };
            if complete {
                break;
            }
        }
        assert_eq!(rgba, vec![0; 360000]);
        assert_eq!(
            chunks,
            rgba.len()
                .div_ceil(first.codec.limits().resource_chunk_bytes)
        );
        let asset = asset.unwrap();
        projection
            .lock()
            .unwrap()
            .install_graphics(asset.clone())
            .unwrap();
        assert_eq!(asset.rgba().unwrap(), rgba);
        // A missing asset terminates only its transfer through reserved lifecycle egress.
        let mut missing_resource = resource;
        missing_resource.key.id = spaceterm_core::content::GraphicsResourceId::new(999).unwrap();
        let missing = GraphicsRequest::new(
            stamp,
            ResourceTransferId::new(8).unwrap(),
            missing_resource,
            0,
        )
        .unwrap();
        first
            .send(match major {
                1 => Payload::Major1(v1::Message {
                    kind: Some(v1::message::Kind::ResourceRequest(
                        content::v1::encode_graphics_request(missing),
                    )),
                }),
                _ => Payload::Major2(v2::Message {
                    kind: Some(v2::message::Kind::ResourceRequest(
                        content::v2::encode_graphics_request(missing),
                    )),
                }),
            })
            .await;
        loop {
            match first.recv().await {
                Payload::Major1(v1::Message {
                    kind: Some(v1::message::Kind::ResourceCancel(value)),
                }) => {
                    assert_eq!(value.transfer, 8);
                    break;
                }
                Payload::Major2(v2::Message {
                    kind: Some(v2::message::Kind::ResourceCancel(value)),
                }) => {
                    assert_eq!(value.transfer, 8);
                    break;
                }
                _ => {}
            }
        }
        // A cancelled view does not kill the owner. A later subscription reads the same Session.
        first.subscribe(original, 1, true).await;
        first.line(5, original, "echo detached-ok").await;
        first.subscribe(original, 2, false).await;
        first.output("detached-ok").await;
        use spaceterm_core::native_requests::{
            NativeCompletionKind, NativeRequestKind, NativeText,
        };
        first
            .line(
                7,
                original,
                r"printf '\033]52;c;c2VjcmV0\033\\'; echo clipboard-write-ok",
            )
            .await;
        let write = first.native_request().await;
        assert!(
            matches!(write.kind(), NativeRequestKind::ClipboardWrite(text) if text.as_str() == "secret")
        );
        assert_eq!(write.fence(), original);
        assert_eq!(
            first
                .complete_native(&write, NativeCompletionKind::ClipboardWritten)
                .await,
            None
        );
        first.output("clipboard-write-ok").await;
        first.line(9, original, r"stty -echo -icanon; printf '\033]52;c;?\033\\'; dd bs=1 count=17 2>/dev/null | od -An -tx1 | tr -d ' \n'; stty echo icanon; printf '\n'").await;
        let read = first.native_request().await;
        assert!(matches!(read.kind(), NativeRequestKind::ClipboardRead));
        assert!(read.deadline().unwrap().get() <= 1000);
        assert_eq!(
            first
                .complete_native(
                    &read,
                    NativeCompletionKind::ClipboardText(NativeText::new("secret".into()).unwrap())
                )
                .await,
            None
        );
        first.output("1b5d35323b633b6332566a636d56301b5c").await;
        // The original one-second deadline survives dispatch and a late completion.
        first.line(11, original, r"stty -echo -icanon; printf '\033]52;c;?\033\\'; dd bs=1 count=9 2>/dev/null | od -An -tx1 | tr -d ' \n'; stty echo icanon; printf '\n'").await;
        let expired = first.native_request().await;
        clock.advance(1050);
        assert_eq!(
            first
                .complete_native(
                    &expired,
                    NativeCompletionKind::ClipboardText(
                        NativeText::new("late-text".into()).unwrap()
                    )
                )
                .await,
            Some(spaceterm_protocol::wire::common::FailureClass::Stale as i32)
        );
        first.output("1b5d35323b633b1b5c").await;
        first.line(13, original, r"printf '\033]7701;permissions=screen-recording,accessibility\033\\'; echo permission-ok").await;
        let permission = first.native_request().await;
        assert!(
            matches!(permission.kind(), NativeRequestKind::Permission(notice) if notice.screen_recording() && notice.accessibility())
        );
        assert_eq!(permission.not_before().get(), 500);
        clock.advance(501);
        assert_eq!(
            first
                .complete_native(&permission, NativeCompletionKind::PermissionAcknowledged)
                .await,
            None
        );
        first.output("permission-ok").await;
        // Notifications carry no program-supplied text and require current native authority.
        first
            .line(15, original, r"printf '\007'; echo bell-ok")
            .await;
        let notification = first.native_request().await;
        assert_eq!(notification.fence(), original);
        assert!(matches!(
            notification.kind(),
            NativeRequestKind::Notification(
                spaceterm_core::native_requests::NotificationNotice::Bell
            )
        ));
        assert_eq!(
            first
                .complete_native(
                    &notification,
                    NativeCompletionKind::NotificationAcknowledged
                )
                .await,
            None
        );
        first.output("bell-ok").await;
        // Observe the start before releasing the finish. A busy PTY worker can
        // otherwise parse both shell markers in one batch and measure zero time.
        first.line(17, original, r"printf '\033]133;C;cmdline=true\007'; echo command-notification-started; while [ ! -f command-notification-ready ]; do sleep 0.01; done; printf '\033]133;D;3\007'; echo command-notification-ok").await;
        first.output("command-notification-started").await;
        clock.advance(100);
        std::fs::write(directory.0.join("command-notification-ready"), []).unwrap();
        let command_notice = first.native_request().await;
        assert!(
            matches!(command_notice.kind(), NativeRequestKind::Notification(spaceterm_core::native_requests::NotificationNotice::CommandFinished { exit_status: Some(3), duration_millis }) if *duration_millis == 100)
        );
        assert_eq!(
            first
                .complete_native(
                    &command_notice,
                    NativeCompletionKind::NotificationAcknowledged
                )
                .await,
            None
        );
        first.output("command-notification-ok").await;
        // Approval is delivered while the original write is pending. A second unsafe paste is refused.
        let pending = FencedRuntimeCommand {
            fence: original,
            command: RuntimeCommand::RequestPaste(
                BoundedPaste::new("echo paste-ok\n".into()).unwrap(),
            ),
        };
        first
            .send(match major {
                1 => Payload::Major1(v1::Message {
                    kind: Some(v1::message::Kind::RuntimeCommand(
                        runtime::v1::encode_command(100, &pending).unwrap(),
                    )),
                }),
                _ => Payload::Major2(v2::Message {
                    kind: Some(v2::message::Kind::RuntimeCommand(
                        runtime::v2::encode_command(100, &pending).unwrap(),
                    )),
                }),
            })
            .await;
        let native = first.native_request().await;
        assert_eq!(
            first
                .effect(
                    101,
                    original,
                    RuntimeCommand::RequestPaste(
                        BoundedPaste::new("echo must-not-paste\n".into()).unwrap()
                    )
                )
                .await,
            Some(spaceterm_protocol::wire::common::FailureClass::Limit as i32)
        );
        let spaceterm_core::native_requests::NativeRequestKind::UnsafePaste { approval, .. } =
            native.kind()
        else {
            panic!("unsafe paste request")
        };
        let completion = spaceterm_core::native_requests::NativeCompletion::new(
            &native,
            spaceterm_core::native_requests::NativeCompletionKind::Paste {
                approval: *approval,
                decision: spaceterm_core::native_requests::PasteDecision::Confirm,
            },
        )
        .unwrap();
        first.outcomes.remove(&native.id().get());
        first
            .send(match major {
                1 => Payload::Major1(v1::Message {
                    kind: Some(v1::message::Kind::NativeCompletion(
                        spaceterm_protocol::convert::native_requests::v1::encode_completion(
                            &completion,
                        )
                        .unwrap(),
                    )),
                }),
                _ => Payload::Major2(v2::Message {
                    kind: Some(v2::message::Kind::NativeCompletion(
                        spaceterm_protocol::convert::native_requests::v2::encode_completion(
                            &completion,
                        )
                        .unwrap(),
                    )),
                }),
            })
            .await;
        let mut settled = false;
        let mut completed = false;
        while !settled || !completed {
            if let Some(failure) = first.outcomes.remove(&100) {
                assert!(failure.is_none());
                settled = true;
            }
            if let Some(failure) = first.outcomes.remove(&native.id().get()) {
                assert!(failure.is_none());
                completed = true;
            }
            if !settled || !completed {
                first.recv().await;
            }
        }
        first.output("paste-ok").await;
        assert!(!first.text.contains("must-not-paste"));
        // IME keeps the Focused Pane exception, unlike ordinary keys.
        let ime_context = original.context().get() + 1;
        first.context(pane, ime_context, false).await;
        let ime_fence = fence(first.principal, session, created.control(), ime_context);
        assert_eq!(
            first
                .effect(
                    102,
                    ime_fence,
                    RuntimeCommand::Key(spaceterm_terminal::KeyInput::text_input("rejected-key"))
                )
                .await,
            Some(spaceterm_protocol::wire::common::FailureClass::Stale as i32)
        );
        assert_eq!(
            first
                .effect(
                    103,
                    ime_fence,
                    RuntimeCommand::InputMethodCommit(
                        BoundedInputMethodText::new("echo ime-ok".into()).unwrap()
                    )
                )
                .await,
            None
        );
        first.context(pane, ime_context + 1, true).await;
        original = fence(first.principal, session, created.control(), ime_context + 1);
        let mut enter = spaceterm_terminal::KeyInput::text_input("");
        enter.physical_key = spaceterm_terminal::PhysicalKey::Enter;
        enter.logical_key = "enter".into();
        enter.text = None;
        assert_eq!(
            first
                .effect(104, original, RuntimeCommand::Key(enter))
                .await,
            None
        );
        first.subscribe(original, 3, false).await;
        first.output("ime-ok").await;
        assert_eq!(
            first
                .effect(
                    105,
                    original,
                    RuntimeCommand::CellPixelMetrics(
                        spaceterm_core::geometry::CellPixels::new(8, 16).unwrap()
                    )
                )
                .await,
            None
        );
        let resized = spaceterm_core::geometry::MeasuredGrid::new(
            29,
            97,
            spaceterm_core::geometry::CellPixels::new(8, 16).unwrap(),
            spaceterm_core::geometry::SUPPORTED_GRID_LIMITS,
        )
        .unwrap();
        assert_eq!(
            first
                .effect(106, original, RuntimeCommand::Resize(resized))
                .await,
            None
        );
        first
            .line(107, original, "printf 'geometry=%s\\n' \"$(stty size)\"")
            .await;
        first.output("geometry=29 97").await;
        assert_eq!(
            first
                .effect(109, original, RuntimeCommand::FocusReport(false))
                .await,
            None
        );
        let mut appearance = actor_fixture::appearance();
        appearance.generation = spaceterm_terminal::appearance::AppearanceGeneration::new(1);
        assert_eq!(
            first
                .effect(
                    110,
                    original,
                    RuntimeCommand::Appearance(Box::new(appearance))
                )
                .await,
            None
        );
        let grant = original.grant();
        let binding = original.connection();
        let mutations = [
            EffectFence::new(
                SessionKey {
                    epoch: ServerEpoch::new(session.epoch.get() + 1),
                    ..session
                },
                grant,
                binding,
                original.context(),
                None,
            )
            .unwrap(),
            EffectFence::new(
                SessionKey {
                    generation: SessionGeneration::new(session.generation.get() + 1),
                    ..session
                },
                grant,
                binding,
                original.context(),
                None,
            )
            .unwrap(),
            EffectFence::new(
                session,
                ControlGrant::new(
                    pane,
                    grant.client(),
                    ControlGeneration::new(grant.generation().get() + 1),
                )
                .unwrap(),
                binding,
                original.context(),
                None,
            )
            .unwrap(),
            EffectFence::new(
                session,
                grant,
                ConnectionBinding::new(
                    binding.client,
                    ConnectionGeneration::new(binding.generation.get() + 1),
                ),
                original.context(),
                None,
            )
            .unwrap(),
            EffectFence::new(
                session,
                grant,
                binding,
                ContextGeneration::new(original.context().get() + 1),
                None,
            )
            .unwrap(),
        ];
        for (index, stale) in mutations.into_iter().enumerate() {
            assert_eq!(
                first
                    .effect(
                        111 + index as u64,
                        stale,
                        RuntimeCommand::Key(spaceterm_terminal::KeyInput::text_input(
                            "fenced-input"
                        ))
                    )
                    .await,
                Some(spaceterm_protocol::wire::common::FailureClass::Stale as i32)
            );
        }
        // Native completions keep their original grant across another client's takeover.
        first
            .line(
                116,
                original,
                r"printf '\033]52;c;c2VjcmV0\033\\'; echo pending-native-ok",
            )
            .await;
        let pending_native = first.native_request().await;
        let mut second = peer(
            &ipc,
            &listener,
            &authenticator,
            dispatcher,
            handle.clone(),
            (keys, handshake),
            (stream2, 44),
        )
        .await;
        second.reconcile(handle.snapshot().state().revision).await;
        second.context(pane, 2, true).await;
        let Outcome::ControlGranted(taken) = second
            .operation(
                1,
                Command::TakeOver(TakeOverRequest {
                    epoch: second.principal.epoch(),
                    pane,
                    client: second.principal.client(),
                }),
            )
            .await
        else {
            panic!("takeover grant")
        };
        assert_eq!(
            first
                .complete_native(&pending_native, NativeCompletionKind::ClipboardWritten)
                .await,
            Some(spaceterm_protocol::wire::common::FailureClass::Stale as i32)
        );
        let snapshot = handle.snapshot();
        channels
            .publish(
                client_info(first.principal, major, snapshot.state().revision),
                client_replica(&snapshot),
            )
            .unwrap();
        assert!(
            !asset.is_paintable(),
            "takeover revokes the exact decoded asset lease"
        );
        let next = fence(second.principal, session, taken, 2);
        assert_eq!(
            first
                .effect(
                    118,
                    original,
                    RuntimeCommand::Key(spaceterm_terminal::KeyInput::text_input(
                        "echo rejected-input"
                    ))
                )
                .await,
            Some(spaceterm_protocol::wire::common::FailureClass::Stale as i32)
        );
        second.subscribe(next, 1, false).await;
        second.line(1, next, "echo takeover-ok").await;
        second.output("takeover-ok").await;
        assert!(!second.text.contains("rejected-input"));
        // The fixture's local file barrier lets output begin only after native
        // eligibility is withdrawn. No timing assumption races the PTY reader.
        second.line(3, next, r"while [ ! -f native-ready ]; do sleep 0.01; done; stty -echo -icanon; printf '\033]52;c;?\033\\'; dd bs=1 count=9 2>/dev/null | od -An -tx1 | tr -d ' \n'; stty echo icanon; printf '\n'; printf '\007\033]7701;permissions=accessibility\033\\'; echo native-denied-ok").await;
        second.context(pane, 3, false).await;
        let ineligible = fence(second.principal, session, taken, 3);
        second.subscribe(ineligible, 2, false).await;
        std::fs::write(directory.0.join("native-ready"), []).unwrap();
        second.output("1b5d35323b633b1b5c").await;
        second.output("native-denied-ok").await;
        assert_eq!(second.native_seen, 0);
        first.close().await;
        second.close().await;
        authenticator.close().await.unwrap();
        stop_physical_server(&handle, host).await;
        server.wait().await.unwrap();
        listener.close().unwrap();
    }
}

#[tokio::test]
async fn authenticated_ipc_one_mib_unsafe_paste_approval() {
    use spaceterm_core::native_requests::{NativeCompletionKind, NativeRequestKind, PasteDecision};
    for major in [1, 2] {
        // Isolate text admission from wall-clock expiry. The deadline and
        // permission-delay vertical above still uses the real epoch clock.
        let fixture =
            started_terminal_with_clock(major, Default::default(), Arc::new(actor_fixture::Clock))
                .await;
        let mut first = fixture.first;
        let original = fence(
            first.principal,
            fixture.session,
            fixture.created.control(),
            2,
        );
        first.subscribe(original, 1, false).await;
        first
            .line(1, original, "PS1=; echo maximum-paste-ready")
            .await;
        first.output("maximum-paste-ready").await;
        let text = format!(
            "echo must-not-run\n#{}",
            "x".repeat(spaceterm_protocol::limits::MAX_PASTE_BYTES - 19)
        );
        assert_eq!(text.len(), spaceterm_protocol::limits::MAX_PASTE_BYTES);
        let pending = FencedRuntimeCommand {
            fence: original,
            command: RuntimeCommand::RequestPaste(BoundedPaste::new(text.clone()).unwrap()),
        };
        first
            .send(match major {
                1 => Payload::Major1(v1::Message {
                    kind: Some(v1::message::Kind::RuntimeCommand(
                        runtime::v1::encode_command(100, &pending).unwrap(),
                    )),
                }),
                _ => Payload::Major2(v2::Message {
                    kind: Some(v2::message::Kind::RuntimeCommand(
                        runtime::v2::encode_command(100, &pending).unwrap(),
                    )),
                }),
            })
            .await;
        let approval = first.native_request().await;
        let NativeRequestKind::UnsafePaste {
            approval: id,
            text: delivered,
        } = approval.kind()
        else {
            panic!("full unsafe paste approval");
        };
        assert!(
            delivered.as_str() == text,
            "unsafe paste text must arrive unchanged and untruncated"
        );
        assert_eq!(
            first
                .complete_native(
                    &approval,
                    NativeCompletionKind::Paste {
                        approval: *id,
                        decision: PasteDecision::Cancel,
                    }
                )
                .await,
            Some(spaceterm_protocol::wire::common::FailureClass::Stale as i32)
        );
        while !first.outcomes.contains_key(&100) {
            first.recv().await;
        }
        assert_eq!(first.outcomes.remove(&100), Some(None));
        first
            .line(101, original, "echo maximum-paste-cancelled")
            .await;
        first.output("maximum-paste-cancelled").await;
        assert!(!first.text.contains("must-not-run"));
        first.close().await;
        fixture.authenticator.close().await.unwrap();
        stop_physical_server(&fixture.handle, fixture.host).await;
        fixture.server.wait().await.unwrap();
        fixture.listener.close().unwrap();
    }
}

#[tokio::test]
async fn authenticated_ipc_one_mib_clipboard_write() {
    use spaceterm_core::native_requests::{NativeCompletionKind, NativeRequestKind};
    for major in [1, 2] {
        // Isolate text admission from wall-clock expiry. The deadline and
        // permission-delay vertical above still uses the real epoch clock.
        let fixture =
            started_terminal_with_clock(major, Default::default(), Arc::new(actor_fixture::Clock))
                .await;
        let mut first = fixture.first;
        let original = fence(
            first.principal,
            fixture.session,
            fixture.created.control(),
            2,
        );
        first.subscribe(original, 1, false).await;
        first
            .line(1, original, "PS1=; echo maximum-clipboard-ready")
            .await;
        first.output("maximum-clipboard-ready").await;
        first
            .line(
                3,
                original,
                r"printf '\033]52;c;'; head -c 1048576 /dev/zero | tr '\000' x | base64 | tr -d '\n'; printf '\033\\\n'; echo maximum-clipboard-done",
            )
            .await;
        let request = first.native_request().await;
        let NativeRequestKind::ClipboardWrite(text) = request.kind() else {
            panic!("clipboard write request");
        };
        assert_eq!(
            text.as_str().len(),
            spaceterm_protocol::limits::MAX_CLIPBOARD_BYTES
        );
        assert!(text.as_str().bytes().all(|byte| byte == b'x'));
        assert_eq!(
            first
                .complete_native(&request, NativeCompletionKind::ClipboardWritten)
                .await,
            None
        );
        first.output("maximum-clipboard-done").await;
        first.close().await;
        fixture.authenticator.close().await.unwrap();
        stop_physical_server(&fixture.handle, fixture.host).await;
        fixture.server.wait().await.unwrap();
        fixture.listener.close().unwrap();
    }
}

#[tokio::test]
async fn authenticated_ipc_end_all_retires_content_and_approvals_without_deleting_organization() {
    use spaceterm_core::native_requests::{NativeCompletionKind, NativeRequestKind, PasteDecision};
    for major in [1, 2] {
        let fixture = started_terminal(major).await;
        let mut first = fixture.first;
        let original = fence(
            first.principal,
            fixture.session,
            fixture.created.control(),
            2,
        );
        first.subscribe(original, 1, false).await;
        first.line(1, original, "PS1=; i=0; while [ $i -lt 100 ]; do echo retired-output-$i; i=$((i+1)); done; echo history-ready").await;
        first.output("history-ready").await;
        first.line(3, original, r"printf '\033_Ga=T,f=32,s=1,v=1,i=7,q=2;AQID/w==\033\\\n'; echo retired-image-ready").await;
        first.output("retired-image-ready").await;
        let (mut old_stamp, resource) = first.resource.expect("live image before retirement");
        let access = fixture
            .handle
            .runtime_access(first.principal, fixture.session)
            .await
            .unwrap();
        let limits = PageLimits::new(256, 16384, 128 * 1024).unwrap();
        loop {
            let request = TopologyPageRequest::new(old_stamp, None, limits).unwrap();
            match access.handle.read_topology(request).unwrap().await.unwrap() {
                ContentReadResult::Ready(topology) => {
                    assert!(topology.rows().len() > 37, "live session has scrollback");
                    break;
                }
                ContentReadResult::ResyncRequired { current, .. } => {
                    old_stamp = QueryStamp::new(
                        EffectFence::new(
                            original.session(),
                            original.grant(),
                            original.connection(),
                            original.context(),
                            Some(current.revision),
                        )
                        .unwrap(),
                        current,
                        old_stamp.request(),
                        old_stamp.generation(),
                    )
                    .unwrap();
                }
                ContentReadResult::Rejected { .. } => panic!("live topology"),
            }
        }
        let pending = FencedRuntimeCommand {
            fence: original,
            command: RuntimeCommand::RequestPaste(
                BoundedPaste::new("echo retired-paste\n".into()).unwrap(),
            ),
        };
        first
            .send(match major {
                1 => Payload::Major1(v1::Message {
                    kind: Some(v1::message::Kind::RuntimeCommand(
                        runtime::v1::encode_command(100, &pending).unwrap(),
                    )),
                }),
                _ => Payload::Major2(v2::Message {
                    kind: Some(v2::message::Kind::RuntimeCommand(
                        runtime::v2::encode_command(100, &pending).unwrap(),
                    )),
                }),
            })
            .await;
        let approval = first.native_request().await;
        let NativeRequestKind::UnsafePaste {
            approval: approval_id,
            ..
        } = approval.kind()
        else {
            panic!("paste approval before retirement");
        };
        let before = fixture.handle.snapshot();
        let Outcome::Assessment(assessment) = first
            .operation(
                4,
                Command::AssessEndAllSessions {
                    server: first.principal.server(),
                },
            )
            .await
        else {
            panic!("End All assessment");
        };
        assert_eq!(assessment.summary().live_session_count(), 1);
        assert_eq!(
            tokio::time::timeout(
                Duration::from_secs(120),
                first.operation(
                    5,
                    Command::EndAllSessions {
                        server: first.principal.server(),
                        assessment: assessment.reference(),
                    }
                )
            )
            .await
            .unwrap(),
            Outcome::Applied
        );
        let ended = fixture.handle.snapshot();
        assert!(
            ended.state().workspaces == before.state().workspaces,
            "End All preserves workspaces"
        );
        assert!(
            ended.state().tabs == before.state().tabs,
            "End All preserves tabs"
        );
        assert!(
            ended.state().panes == before.state().panes,
            "End All preserves panes"
        );
        assert_eq!(ended.runtime()[0].control, before.runtime()[0].control);
        assert_eq!(
            ended.runtime()[0].phase,
            SessionPhase::Ended {
                reason: EndReason::EndAllSessions
            }
        );
        assert_eq!(
            first
                .complete_native(
                    &approval,
                    NativeCompletionKind::Paste {
                        approval: *approval_id,
                        decision: PasteDecision::Confirm,
                    }
                )
                .await,
            Some(spaceterm_protocol::wire::common::FailureClass::Stale as i32)
        );
        assert!(
            fixture
                .handle
                .runtime_access(first.principal, fixture.session)
                .await
                .is_err()
        );
        let old_read = access.handle.read_page(
            ContentPageRequest::new(old_stamp, ContentRange::ActiveGrid, None, limits).unwrap(),
        );
        if let Ok(read) = old_read {
            assert!(
                !matches!(read.await, Ok(ContentReadResult::Ready(_))),
                "retired accessibility rows are unavailable"
            );
        }
        let old_graphics =
            GraphicsRequest::new(old_stamp, ResourceTransferId::new(1).unwrap(), resource, 0)
                .unwrap();
        if let Ok(mut chunks) = access.handle.read_graphics(old_graphics) {
            assert!(
                !matches!(chunks.next().await, Some(Ok(_))),
                "retired image bytes are unavailable"
            );
        }
        first.reconcile(ended.state().revision).await;
        let pre_session = SessionKey {
            generation: ended.runtime()[0].session_generation,
            ..fixture.session
        };
        let startup_fence = fence(first.principal, pre_session, fixture.created.control(), 2);
        let intent = StartupIntent::new(
            OperationId::new(first.stream, OperationSequence::new(6).unwrap()),
            startup_fence,
            StartupSource::ExplicitSelection,
        )
        .unwrap();
        assert!(matches!(
            first.operation(6, Command::BeginStartup(intent)).await,
            Outcome::StartupAwaitingMeasurement { .. }
        ));
        let grid = spaceterm_core::geometry::MeasuredGrid::new(
            37,
            113,
            spaceterm_core::geometry::CellPixels::new(9, 18).unwrap(),
            spaceterm_core::geometry::SUPPORTED_GRID_LIMITS,
        )
        .unwrap();
        let Outcome::SessionStarted(replacement) = first
            .operation(
                7,
                Command::CompleteMeasuredStartup(MeasuredStartupMeasurement {
                    intent: intent.id(),
                    fence: startup_fence,
                    grid,
                }),
            )
            .await
        else {
            panic!("explicit replacement session");
        };
        assert_ne!(replacement.generation, fixture.session.generation);
        let mut snapshots = fixture.handle.subscribe_snapshot();
        tokio::time::timeout(Duration::from_secs(15), async {
            while !matches!(snapshots.borrow().runtime()[0].phase, SessionPhase::Running { generation } if generation == replacement.generation) {
                snapshots.changed().await.unwrap();
            }
        }).await.unwrap();
        let next = fence(first.principal, replacement, fixture.created.control(), 2);
        first.text.clear();
        first.pages.clear();
        first.resource = None;
        first.subscribe(next, 2, false).await;
        first.line(101, next, "PS1=; echo replacement-ready").await;
        first.output("replacement-ready").await;
        assert!(!first.text.contains("retired-output"));
        assert!(!first.text.contains("retired-paste"));
        assert!(
            first
                .pages
                .iter()
                .all(|page| page.parts().resources.is_empty())
        );
        let base = first.pages.last().unwrap().parts().base;
        let stamp = QueryStamp::new(
            EffectFence::new(
                next.session(),
                next.grant(),
                next.connection(),
                next.context(),
                Some(base.revision),
            )
            .unwrap(),
            base,
            ContentRequestId::new(200).unwrap(),
            RequestGeneration::new(1),
        )
        .unwrap();
        let access = fixture
            .handle
            .runtime_access(first.principal, replacement)
            .await
            .unwrap();
        let ContentReadResult::Ready(topology) = access
            .handle
            .read_topology(TopologyPageRequest::new(stamp, None, limits).unwrap())
            .unwrap()
            .await
            .unwrap()
        else {
            panic!("replacement topology");
        };
        assert_eq!(
            topology.rows().len(),
            37,
            "replacement starts without old scrollback"
        );
        first.close().await;
        fixture.authenticator.close().await.unwrap();
        stop_physical_server(&fixture.handle, fixture.host).await;
        fixture.server.wait().await.unwrap();
        fixture.listener.close().unwrap();
    }
}

#[tokio::test]
async fn authenticated_ipc_oversized_negotiated_clipboard_request_is_denied_without_disconnect() {
    for major in [1, 2] {
        for (limits, source_bytes) in [
            (
                spaceterm_protocol::limits::NegotiatedLimits {
                    clipboard_bytes: 16,
                    ..Default::default()
                },
                32,
            ),
            (
                spaceterm_protocol::limits::NegotiatedLimits {
                    frame_bytes: 16 * 1024,
                    management_bytes: 16 * 1024,
                    ..Default::default()
                },
                32 * 1024,
            ),
        ] {
            let fixture = started_terminal_with_limits(major, limits).await;
            let mut first = fixture.first;
            let original = fence(
                first.principal,
                fixture.session,
                fixture.created.control(),
                2,
            );
            first.subscribe(original, 1, false).await;
            first
                .line(1, original, "PS1=; echo negotiated-clipboard-ready")
                .await;
            first.output("negotiated-clipboard-ready").await;
            first.line(3, original, &format!(r"printf '\033]52;c;'; head -c {source_bytes} /dev/zero | tr '\000' x | base64 | tr -d '\n'; printf '\033\\\n'; echo negotiated-clipboard-denied")).await;
            first.output("negotiated-clipboard-denied").await;
            first
                .line(5, original, "echo negotiated-clipboard-still-connected")
                .await;
            first.output("negotiated-clipboard-still-connected").await;
            assert_eq!(first.native_seen, 0);
            first.close().await;
            fixture.authenticator.close().await.unwrap();
            stop_physical_server(&fixture.handle, fixture.host).await;
            fixture.server.wait().await.unwrap();
            fixture.listener.close().unwrap();
        }
    }
}
