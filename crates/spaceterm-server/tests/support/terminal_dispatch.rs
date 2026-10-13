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
use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Duration};

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
    async fn output(&mut self, expected: &str) {
        tokio::time::timeout(Duration::from_secs(15), async {
            while !self.text.lines().any(|line| line.trim() == expected) {
                self.recv().await;
            }
        })
        .await
        .expect("semantic subscription reaches shell output");
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

#[tokio::test]
async fn authenticated_ipc_terminal_input_pages_and_two_client_takeover_both_majors() {
    for major in [1, 2] {
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
        let clock = Arc::new(NativeRuntimeClock::default());
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
            capabilities: vec![
                spaceterm_protocol::negotiation::CAPABILITY_CANONICAL_DOMAIN,
                spaceterm_protocol::negotiation::CAPABILITY_TERMINAL_STREAMS,
            ],
            ..Default::default()
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
        let mut original = fence(first.principal, session, created.control(), 2);
        first.subscribe(original, 1, false).await;
        first.line(1, original, "echo spaceterm-ok").await;
        first.output("spaceterm-ok").await;
        assert!(
            first.parts_seen > 1,
            "bounded semantic transfer must be multipart"
        );
        // A cancelled view does not kill the owner. A later subscription reads the same Session.
        first.subscribe(original, 1, true).await;
        first.line(3, original, "echo detached-ok").await;
        first.subscribe(original, 2, false).await;
        first.output("detached-ok").await;
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
        let native = loop {
            match first.recv().await {
                Payload::Major1(v1::Message {
                    kind: Some(v1::message::Kind::NativeRequest(value)),
                }) => {
                    break spaceterm_protocol::convert::native_requests::v1::decode_request(
                        &value,
                        spaceterm_core::observations::MonotonicTick::new(0),
                    )
                    .unwrap();
                }
                Payload::Major2(v2::Message {
                    kind: Some(v2::message::Kind::NativeRequest(value)),
                }) => {
                    break spaceterm_protocol::convert::native_requests::v2::decode_request(
                        &value,
                        spaceterm_core::observations::MonotonicTick::new(0),
                    )
                    .unwrap();
                }
                _ => {}
            }
        };
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
        first.context(pane, 3, false).await;
        let ime_fence = fence(first.principal, session, created.control(), 3);
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
        first.context(pane, 4, true).await;
        original = fence(first.principal, session, created.control(), 4);
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
        let next = fence(second.principal, session, taken, 2);
        assert_eq!(
            first
                .effect(
                    116,
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
        first.close().await;
        second.close().await;
        authenticator.close().await.unwrap();
        stop_physical_server(&handle, host).await;
        server.wait().await.unwrap();
        listener.close().unwrap();
    }
}
