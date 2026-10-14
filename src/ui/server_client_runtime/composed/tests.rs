use super::*;
use crate::ui::server_client_runtime::{
    DesktopClientDependencies, DesktopClientRuntime, notification_channel,
};
use gpui::TestAppContext;
use spaceterm_client::journal::ClientJournal;
use spaceterm_client::{
    api::{CompatibilityExplanation, ConnectionState},
    failure::{ClientFailure, ClientFailureClass},
    pending::LocalUseInvalidator,
    ports::ClientFuture,
    testing::{FakePaneConnection, FakeServer},
    view::{EncodedView, StoredView, ViewCommit, ViewStoreFailure},
};
use spaceterm_core::{
    authority::GrantedControl,
    geometry::{CellPixels, MeasuredGrid, SUPPORTED_GRID_LIMITS},
    operations::WorkspaceCreationSource,
    organization::NameDraft,
    pairing::LocalPairingKey,
};

#[test]
fn composed_offline_cause_preserves_deliberate_stop_and_incompatibility() {
    let name = DisplayName::reported("Studio Mac");
    assert_eq!(
        offline_cause(&ConnectionState::Stopped, name.clone(), None),
        OfflineCause::Stopped(name.clone())
    );
    assert_eq!(
        offline_cause(
            &ConnectionState::Incompatible {
                explanation: CompatibilityExplanation::NoCommonMajor
            },
            name,
            None
        ),
        OfflineCause::UpdateRequired
    );
}

#[test]
fn composed_blocked_cause_preserves_the_original_intended_action() {
    let connection = ConnectionState::Connecting {
        phase: spaceterm_client::api::ConnectingPhase::Dialing,
    };
    for intended in [IntendedAction::Connect, IntendedAction::Start] {
        assert_eq!(
            offline_cause(
                &connection,
                DisplayName::unnamed(),
                Some((BlockedLocation::ThisComputer, intended))
            ),
            OfflineCause::Blocked {
                server: DisplayName::unnamed(),
                location: BlockedLocation::ThisComputer,
                intended
            }
        );
    }
}

#[derive(Default)]
pub(super) struct MemoryView {
    version: u64,
    bytes: Option<Vec<u8>>,
}
impl ViewStore for MemoryView {
    type Version = u64;
    fn load(&mut self, _: usize) -> Result<Option<StoredView<u64>>, ViewStoreFailure> {
        Ok(self.bytes.clone().map(|bytes| StoredView {
            version: self.version,
            bytes,
        }))
    }
    fn replace(&mut self, expected: Option<&u64>, document: &EncodedView) -> ViewCommit<u64> {
        assert_eq!(expected.copied(), self.bytes.as_ref().map(|_| self.version));
        self.version += 1;
        self.bytes = Some(document.as_bytes().to_vec());
        ViewCommit::Durable(self.version)
    }
}

pub(super) struct Invalidator(pub(super) Arc<PaneChannels>);
impl LocalUseInvalidator for Invalidator {
    fn invalidate_workspace(
        &self,
        _: &LocalPairingKey,
        _: WorkspaceId,
    ) -> Result<Vec<GrantedControl>, ClientFailure> {
        Err(ClientFailure::new(ClientFailureClass::StaleAuthority))
    }
    fn invalidate_attachment(
        &self,
        pairing: &LocalPairingKey,
        attachment: view::WorkspaceAttachment,
    ) -> Result<Vec<GrantedControl>, ClientFailure> {
        self.0.invalidate_attachment(pairing, attachment)
    }
    fn invalidate_pairing(&self, _: &LocalPairingKey) {}
    fn remove_pairing<'a>(&'a self, _: &'a LocalPairingKey) -> ClientFuture<'a, ()> {
        Box::pin(async { Err(ClientFailure::new(ClientFailureClass::Unavailable)) })
    }
}

struct Harness {
    tokio: tokio::runtime::Runtime,
    server: Arc<FakeServer>,
    peer: Arc<FakePaneConnection>,
    provider: Rc<ComposedDesktopClientProvider<MemoryView>>,
    runtime: gpui::Entity<DesktopClientRuntime>,
    connection: watch::Sender<ConnectionState>,
    replica: watch::Sender<ReplicaState>,
    pane: PaneId,
    workspace: WorkspaceId,
    tab: TabId,
    other: Option<(
        ServerId,
        WorkspaceId,
        watch::Sender<ConnectionState>,
        watch::Sender<ReplicaState>,
    )>,
}
impl Harness {
    fn new(cx: &mut TestAppContext) -> Self {
        Self::build(cx, false)
    }
    fn build(cx: &mut TestAppContext, multiple: bool) -> Self {
        let tokio = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let fixture = tokio.block_on(crate::ui::server_pane_session::tests::Fixture::new(false));
        let snapshot = fixture.server.replica().unwrap();
        let info = fixture.channel.state().borrow().connection.unwrap();
        let binding = fixture.server.binding(1).unwrap();
        let observation = ReplicaObservation::new(
            &snapshot.organization,
            snapshot.accepted_creations(),
            ReplicaFacts {
                pairing: binding.target().pairing(),
                client: info.client,
                connection: Some(info.binding),
                readiness: ReplicaReadiness::Ready,
                status: ServerViewStatus::Connected,
            },
        )
        .unwrap();
        let mut view = ClientViewState::new();
        let workspace = snapshot.organization.state().workspaces[0].id;
        let pane = snapshot.organization.state().panes[0].id;
        let tab = snapshot.organization.state().tabs[0].id;
        view.attach(workspace, &observation).unwrap();
        let (connection, state) = watch::channel(ConnectionState::Connected { info });
        let (replica, replicas) = watch::channel(ReplicaState::Ready { snapshot });
        let (notifier, notifications) = notification_channel();
        let journal = fixture.peer.journal().clone();
        tokio
            .block_on(journal.record_attachment(
                binding.target().local_pairing().unwrap(),
                view.workspace(workspace).unwrap().attachment(),
            ))
            .unwrap();
        journal
            .bind_pane_channels(fixture.channels.as_ref().clone())
            .unwrap();
        let mut sources = BTreeMap::new();
        sources.insert(
            info.server,
            Source {
                binding,
                name: DesktopServerInfo {
                    server: info.server,
                    name: DisplayName::reported("Studio Mac"),
                    local: true,
                },
                state,
                replica: replicas,
                handle: None,
            },
        );
        let mut other = None;
        let mut order = vec![info.server];
        if multiple {
            let server = FakeServer::new(7, vec![2, 1]).unwrap();
            let peer = tokio
                .block_on(FakePaneConnection::new(server.clone(), 1, 2))
                .unwrap();
            tokio
                .block_on(peer.submit(
                    Command::CreateWorkspace {
                        server: server.server(),
                        source: WorkspaceCreationSource::ServerHome {
                            server: server.server(),
                        },
                        name: NameDraft::Automatic,
                    },
                    None,
                ))
                .unwrap();
            let saved = peer.journal().snapshot().unwrap().bindings()[0].clone();
            tokio
                .block_on(journal.transact(vec![
                    spaceterm_client::journal::ClientJournalChange::SaveBinding(Box::new(saved)),
                ]))
                .unwrap();
            let snapshot = server.replica().unwrap();
            let connected = server.connected_info(1, 1, 1, 2).unwrap();
            let binding = server.binding(1).unwrap();
            let facts = ReplicaFacts {
                pairing: binding.target().pairing(),
                client: connected.client,
                connection: Some(connected.binding),
                readiness: ReplicaReadiness::Ready,
                status: ServerViewStatus::Connected,
            };
            let observation = ReplicaObservation::new(
                &snapshot.organization,
                snapshot.accepted_creations(),
                facts,
            )
            .unwrap();
            let second_workspace = snapshot.organization.state().workspaces[0].id;
            view.attach(second_workspace, &observation).unwrap();
            let (connection, state) =
                watch::channel(ConnectionState::Connected { info: connected });
            let (replica, replicas) = watch::channel(ReplicaState::Ready { snapshot });
            sources.insert(
                server.server(),
                Source {
                    binding,
                    name: DesktopServerInfo {
                        server: server.server(),
                        name: DisplayName::reported("Build server"),
                        local: false,
                    },
                    state,
                    replica: replicas,
                    handle: None,
                },
            );
            order.insert(0, server.server());
            other = Some((server.server(), second_workspace, connection, replica));
        }
        let mut state = State {
            view,
            ..State::default()
        };
        // The fixture's original lease owns this authenticated channel, as production's
        // attachment owner does. No local test Terminal stands in for a server Session.
        state.leases.insert(workspace, fixture.lease);
        state.channels.insert(pane, fixture.channel);
        let inner = Rc::new(Inner {
            git: cx.update(crate::ui::server_git::InstalledServerGit::install),
            git_publications: RefCell::new(BTreeMap::new()),
            sources: RefCell::new(sources),
            order: RefCell::new(order),
            channels: fixture.channels.clone(),
            tracker: OperationTracker::new(journal.clone()),
            operations: fixture.server.clone(),
            pending: PendingTasks::new(journal, Arc::new(Invalidator(fixture.channels))),
            runtime: RefCell::new(None),
            executor: tokio.handle().clone(),
            notifier,
            state: RefCell::new(state),
            tasks: RefCell::new(Vec::new()),
            edits: Arc::new(tokio::sync::Mutex::new(())),
        });
        let mut persistence = ViewPersistence::new(MemoryView::default());
        persistence.load().unwrap();
        let provider = Rc::new(ComposedDesktopClientProvider {
            inner,
            persistence: Arc::new(Mutex::new(persistence)),
        });
        cx.update(|cx| provider.inner.install(cx).unwrap());
        let runtime = cx.new(|cx| {
            DesktopClientRuntime::new(
                DesktopClientDependencies {
                    provider: provider.clone(),
                    notifications,
                },
                cx,
            )
        });
        Self {
            tokio,
            server: fixture.server,
            peer: fixture.peer,
            provider,
            runtime,
            connection,
            replica,
            pane,
            workspace,
            tab,
            other,
        }
    }
    fn publish(&self, cx: &mut TestAppContext) {
        self.peer.publish().unwrap();
        let info = self.server.connected_info(1, 1, 1, 2).unwrap();
        self.replica.send_replace(ReplicaState::Ready {
            snapshot: self.server.replica().unwrap(),
        });
        self.connection
            .send_replace(ConnectionState::Connected { info });
        cx.run_until_parked();
    }
    fn request(&self, request: DesktopRequest, cx: &mut TestAppContext) {
        let task = cx.update(|cx| self.provider.begin(request, cx)).unwrap();
        finish(task, &self.tokio, cx).unwrap();
    }
}
#[track_caller]
pub(super) fn finish<T: 'static>(
    task: Task<T>,
    runtime: &tokio::runtime::Runtime,
    cx: &mut TestAppContext,
) -> T {
    let result = Rc::new(RefCell::new(None));
    let target = result.clone();
    let owned = cx.foreground_executor().spawn(async move {
        *target.borrow_mut() = Some(task.await);
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while result.borrow().is_none() {
        cx.run_until_parked();
        runtime.block_on(tokio::task::yield_now());
        assert!(
            std::time::Instant::now() < deadline,
            "the composed client task did not finish"
        );
    }
    cx.foreground_executor().block_test(owned);
    result.borrow_mut().take().unwrap()
}
fn grid() -> MeasuredGrid {
    MeasuredGrid::new(
        3,
        5,
        CellPixels::new(10, 20).unwrap(),
        SUPPORTED_GRID_LIMITS,
    )
    .unwrap()
}

// The peer is the uncontrolled boundary. Authentication, the registry, native Pane
// channels, startup ownership and the GPUI watcher all remain production owners.
mod multipart_startup {
    use super::*;
    use crate::platform::desktop_client_startup::{
        self, AcceptanceDecoder, AcceptanceProtocol, NativeDesktopOwners,
    };
    use crate::ui::server_client_runtime::testing::wire;
    use spaceterm_client::{
        api::{ConnectedInfo, ConnectionTarget},
        pane_channel::EffectKind,
        ports::ClientServices,
    };
    use spaceterm_core::organization::OrganizationSnapshot;
    use spaceterm_protocol::{
        codec::Payload,
        convert::{
            identity, operations,
            organization::{self, PublicationPart},
        },
        wire::{v1, v2},
    };
    use std::sync::OnceLock;

    static SNAPSHOTS: OnceLock<Mutex<BTreeMap<ServerId, Arc<OrganizationSnapshot>>>> =
        OnceLock::new();

    fn snapshot(server: ServerId) -> Arc<OrganizationSnapshot> {
        SNAPSHOTS.get().unwrap().lock().unwrap()[&server].clone()
    }

    fn publication(request: &Payload, target: &ConnectionTarget) -> Option<Payload> {
        let snapshot = snapshot(target.server());
        macro_rules! profile {
            ($message:expr, $p:ident, $variant:ident) => {{
                match $message.kind.as_ref()? {
                    $p::message::Kind::OrganizationQuery(query) => Some(
                        organization::$p::snapshot_payload(
                            &snapshot,
                            0..organization::record_count(&snapshot),
                            PublicationPart {
                                request: query.request,
                                generation: query.request_generation,
                                transfer: query.request,
                                index: 0,
                                total: 1,
                                epoch: snapshot.epoch(),
                                base: OrganizationRevision::new(0),
                                revision: snapshot.state().revision,
                            },
                        )
                        .unwrap(),
                    ),
                    $p::message::Kind::ClientContext(context) => {
                        let mut acknowledged = context.clone();
                        assert!(!acknowledged.acknowledged);
                        acknowledged.acknowledged = true;
                        Some(Payload::$variant($p::Message {
                            kind: Some($p::message::Kind::ClientContext(acknowledged)),
                        }))
                    }
                    $p::message::Kind::Operation(value) => {
                        let request = operations::$p::decode_request(
                            value,
                            target.pairing(),
                            target.client(),
                            &spaceterm_core::operation_ledger::canonical::CanonicalCodec,
                        )
                        .unwrap();
                        let outcome = match request.command() {
                            Command::BeginStartup(intent) => Outcome::StartupAwaitingMeasurement {
                                intent: intent.id(),
                            },
                            Command::CompleteMeasuredStartup(measurement) => {
                                let mut session = measurement.fence.session();
                                session.generation =
                                    session.generation.checked_successor().unwrap();
                                Outcome::SessionStarted(session)
                            }
                            Command::CancelStartup { .. } => Outcome::Applied,
                            other => panic!("unexpected startup peer command: {other:?}"),
                        };
                        Some(Payload::$variant($p::Message {
                            kind: Some($p::message::Kind::Outcome(
                                operations::$p::encode_outcome(
                                    request.binding().id,
                                    snapshot.state().revision,
                                    &outcome,
                                )
                                .unwrap(),
                            )),
                        }))
                    }
                    $p::message::Kind::Acknowledgement(value) => {
                        let (stream, through) = operations::$p::decode_acknowledgement(
                            value,
                            target.pairing(),
                            target.client(),
                        )
                        .unwrap();
                        Some(Payload::$variant($p::Message {
                            kind: Some($p::message::Kind::AcknowledgementConfirmation(
                                $p::AcknowledgementConfirmation {
                                    stream: Some(identity::encode_stream(stream)),
                                    acknowledged_through: through.get(),
                                },
                            )),
                        }))
                    }
                    _ => None,
                }
            }};
        }
        match request {
            Payload::Major1(message) => profile!(message, v1, Major1),
            Payload::Major2(message) => profile!(message, v2, Major2),
        }
    }

    struct MeasuredPane {
        terminal: gpui::Entity<crate::ui::TerminalPane>,
        received: Rc<RefCell<Vec<PaneMeasurement>>>,
    }
    impl MeasuredPane {
        #[track_caller]
        fn measure(
            &self,
            request: PaneMeasurementRequest,
            cx: &mut TestAppContext,
        ) -> PaneMeasurement {
            let before = self.received.borrow().len();
            self.terminal.update(cx, |pane, cx| {
                pane.request_measurement(request.token, request.ticket, request.layout, cx)
                    .unwrap();
            });
            cx.run_until_parked();
            let received = self.received.borrow();
            let [measured] = &received[before..] else {
                panic!("the real Pane must emit exactly one requested measurement");
            };
            assert_eq!(measured.token, request.token);
            assert_eq!(measured.ticket, request.ticket);
            assert_eq!(measured.layout, request.layout);
            *measured
        }
    }

    struct Fixture {
        executor: tokio::runtime::Runtime,
        provider: Rc<ComposedDesktopClientProvider<MemoryView>>,
        _runtime: gpui::Entity<DesktopClientRuntime>,
        handle: ServerHandle,
        plan: Arc<wire::peer::PeerPlan>,
        binding: PairedServerBinding,
        snapshot: Arc<OrganizationSnapshot>,
        publication_snapshot: Arc<OrganizationSnapshot>,
        pane: PaneId,
        info: ConnectedInfo,
    }
    impl Fixture {
        fn new(major: u32, seed: u8, cx: &mut TestAppContext) -> Self {
            Self::build(major, seed, false, cx)
        }
        fn build(major: u32, seed: u8, additional_tab: bool, cx: &mut TestAppContext) -> Self {
            // GPUI's deterministic foreground scheduler must be woken on its test thread.
            let executor = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let (composition, handle, binding, snapshot, plan) = executor.block_on(async {
                let server = FakeServer::new(seed, vec![major]).unwrap();
                let peer = FakePaneConnection::new(server.clone(), 1, major)
                    .await
                    .unwrap();
                peer.submit(
                    Command::CreateWorkspace {
                        server: server.server(),
                        source: WorkspaceCreationSource::ServerHome {
                            server: server.server(),
                        },
                        name: NameDraft::Automatic,
                    },
                    None,
                )
                .await
                .unwrap();
                // This fixture isolates publication during an already authorized use.
                // Acquire through the public peer before capturing the authenticated snapshot.
                let unclaimed = server.snapshot().unwrap();
                let pane = unclaimed.state().panes[0].id;
                let workspace = unclaimed.state().workspaces[0].id;
                if unclaimed.runtime()[0].control
                    == spaceterm_core::organization::ControlRecord::Unclaimed
                {
                    let acquired = peer
                        .submit(
                            Command::CompareAcquire(
                                spaceterm_core::control::CompareAcquireRequest {
                                    epoch: unclaimed.epoch(),
                                    pane,
                                    client: server.binding(1).unwrap().target().client(),
                                    expected_generation: unclaimed.runtime()[0].control_generation,
                                },
                            ),
                            Some(workspace),
                        )
                        .await
                        .unwrap();
                    assert!(
                        matches!(
                            *acquired.status().borrow(),
                            OperationStatus::Completed {
                                outcome: Outcome::ControlGranted(_) | Outcome::ControlAcquired(_),
                                ..
                            } | OperationStatus::Retired
                        ),
                        "public control acquisition did not complete: {:?}",
                        *acquired.status().borrow()
                    );
                }
                if additional_tab {
                    peer.submit(
                        Command::CreateTab {
                            workspace,
                            source: spaceterm_core::operations::TabCreationSource::Pane(pane),
                        },
                        Some(workspace),
                    )
                    .await
                    .unwrap();
                }
                let snapshot = Arc::new(server.snapshot().unwrap());
                assert!(matches!(
                    snapshot.runtime()[0].control,
                    spaceterm_core::organization::ControlRecord::Held { client, .. }
                        if client == server.binding(1).unwrap().target().client()
                ));
                SNAPSHOTS
                    .get_or_init(Default::default)
                    .lock()
                    .unwrap()
                    .insert(server.server(), snapshot.clone());
                let binding = server.binding(1).unwrap();
                let journal = peer.journal().clone();
                let tracker = OperationTracker::new(journal.clone());
                let protocol = Arc::new(AcceptanceProtocol::new(journal.clone(), tracker.clone()));
                let channels = Arc::new(
                    PaneChannels::new(
                        protocol.clone(),
                        spaceterm_client::projection::ProjectionBudgets::supported().unwrap(),
                    )
                    .unwrap(),
                );
                protocol.set_channels(channels.clone()).unwrap();
                let mut plan = wire::peer::PeerPlan::new(vec![major]);
                plan.query_handler = Some(publication);
                plan.capabilities.extend([
                    spaceterm_protocol::negotiation::CAPABILITY_ORGANIZATION_COMPARISONS,
                    spaceterm_protocol::negotiation::CAPABILITY_TERMINAL_STREAMS,
                ]);
                let plan = Arc::new(plan);
                let composition = desktop_client_startup::recover(
                    NativeDesktopOwners {
                        #[cfg(all(feature = "desktop-test-support", debug_assertions))]
                        fixture: false,
                        services: ClientServices {
                            routes: Arc::new(wire::peer::Peers::one(&binding, plan.clone())),
                            credentials: Arc::new(wire::Credentials),
                            journal: journal.clone(),
                            organization: Arc::new(AcceptanceDecoder),
                            delegate: protocol.clone(),
                            discovery: None,
                            limits: Default::default(),
                            additional_capabilities:
                                desktop_client_startup::NATIVE_PROTOCOL_CAPABILITIES.to_vec(),
                        },
                        journal,
                        channels,
                        operations: protocol.clone(),
                        invalidator: protocol.clone(),
                        local_server: Some(server.server()),
                        tracker: Some(tracker),
                        registry_binding: Some(protocol),
                        settings: Default::default(),
                    },
                    MemoryView::default(),
                )
                .await
                .unwrap();
                let handle = composition.registry.server(server.server()).unwrap();
                (composition, handle, binding, snapshot, plan)
            });
            let (notifier, notifications) = notification_channel();
            let provider = cx.update(|cx| {
                Rc::new(ComposedDesktopClientProvider::new(composition, notifier, cx).unwrap())
            });
            // Attachment publishes a durable personal-edit receipt before it completes.
            // The production runtime must own its receiver for the fixture's entire use.
            let runtime = cx.new(|cx| {
                DesktopClientRuntime::new(
                    DesktopClientDependencies {
                        provider: provider.clone(),
                        notifications,
                    },
                    cx,
                )
            });
            executor.block_on(handle.connect()).unwrap();
            let info = executor.block_on(async {
                let mut state = handle.state();
                tokio::time::timeout(std::time::Duration::from_secs(10), async {
                    loop {
                        let current = state.borrow_and_update().clone();
                        if let ConnectionState::Connected { info } = current {
                            break info;
                        }
                        state.changed().await.unwrap();
                    }
                })
                .await
                .unwrap()
            });
            let pane = snapshot.state().panes[0].id;
            let mut publication_state = snapshot.state().clone();
            publication_state.revision = publication_state.revision.checked_successor().unwrap();
            let publication_snapshot = Arc::new(
                OrganizationSnapshot::new(
                    snapshot.epoch(),
                    publication_state,
                    snapshot.runtime().to_vec(),
                )
                .unwrap(),
            );
            let fixture = Self {
                executor,
                provider,
                _runtime: runtime,
                handle,
                plan,
                binding,
                snapshot,
                publication_snapshot,
                pane,
                info,
            };
            fixture.wait(cx, || {
                fixture.provider.frame(pane.server()).is_some_and(|frame| {
                    matches!(frame.connection, ConnectionState::Connected { info: shown } if shown == info)
                })
            });
            fixture.request(
                DesktopRequest::AttachWorkspace(fixture.snapshot.state().workspaces[0].id),
                cx,
            );
            fixture
        }
        fn terminal_pane(&self, cx: &mut TestAppContext) -> MeasuredPane {
            let channel = self.provider.pane_channel(self.pane).unwrap();
            let delivery = self.provider.pane_presentation(self.pane, None).unwrap();
            let presentation = crate::ui::pane_presentation::PanePresentation::from_portable(
                delivery.decision.presentation,
                delivery.device,
                delivery.cause,
            );
            let (terminal, visual) = cx.add_window_view(|window, cx| {
                crate::ui::pane_lifecycle::PaneConstruction::testing()
                    .create_server(channel, delivery.token, presentation, window, cx)
                    .unwrap()
            });
            visual.update(|window, _| window.activate_window());
            visual.run_until_parked();
            let received = Rc::new(RefCell::new(Vec::new()));
            let collected = received.clone();
            terminal.update(cx, |_, cx| {
                cx.subscribe(
                    &terminal,
                    move |_, _, event: &crate::ui::TerminalPaneEvent, _| {
                        if let crate::ui::TerminalPaneEvent::Measured(measurement) = event {
                            collected.borrow_mut().push(*measurement);
                        }
                    },
                )
                .detach();
            });
            MeasuredPane { terminal, received }
        }
        #[track_caller]
        fn start(&self, cx: &mut TestAppContext) {
            self.request(
                DesktopRequest::StartSession {
                    pane: self.pane,
                    source: SelectionSource::Pointer,
                },
                cx,
            );
            self.wait(cx, || self.provider.pane_measurement(self.pane).is_some());
        }
        fn drive(&self, cx: &mut TestAppContext) {
            for _ in 0..10 {
                cx.run_until_parked();
                self.executor.block_on(async {
                    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
                });
            }
            cx.run_until_parked();
        }
        #[track_caller]
        fn wait(&self, cx: &mut TestAppContext, ready: impl Fn() -> bool) {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while !ready() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "multipart desktop did not settle"
                );
                self.drive(cx);
            }
        }
        #[track_caller]
        fn request(&self, request: DesktopRequest, cx: &mut TestAppContext) {
            let task = cx.update(|cx| self.provider.begin(request, cx)).unwrap();
            finish(task, &self.executor, cx).unwrap();
            self.drive(cx);
        }
        fn interleaved_delta(&self) -> Payload {
            let base = self.snapshot.state().revision;
            let revision = base.checked_successor().unwrap();
            let part = PublicationPart {
                request: 1,
                generation: 1,
                transfer: 3,
                index: 0,
                total: 1,
                epoch: self.snapshot.epoch(),
                base,
                revision,
            };
            macro_rules! profile {
                ($p:ident, $variant:ident) => {
                    Payload::$variant($p::Message {
                        kind: Some($p::message::Kind::OrganizationDelta(
                            $p::OrganizationDelta {
                                server: Some(identity::encode_global(self.pane.server().bytes())),
                                epoch: part.epoch.get(),
                                base_revision: base.get(),
                                revision: revision.get(),
                                complete: true,
                                part: Some(organization::$p::encode_part(part)),
                                ..Default::default()
                            },
                        )),
                    })
                };
            }
            let payload = if self.info.major == 1 {
                profile!(v1, Major1)
            } else {
                profile!(v2, Major2)
            };
            assert!(matches!(
                spaceterm_client::ports::OrganizationDecoder::decode(
                    &AcceptanceDecoder,
                    payload.clone(),
                )
                .unwrap(),
                spaceterm_client::ports::OrganizationPublication::DeltaPart { .. }
            ));
            payload
        }
        fn published_info(&self) -> ConnectedInfo {
            ConnectedInfo {
                organization_revision: self.publication_snapshot.state().revision,
                ..self.info
            }
        }
        fn part(&self, index: u32) -> Payload {
            let count = organization::record_count(&self.publication_snapshot);
            assert!(count > 1);
            let range = if index == 0 {
                0..count / 2
            } else {
                count / 2..count
            };
            let part = PublicationPart {
                request: 1,
                generation: 1,
                transfer: 2,
                index,
                total: 2,
                epoch: self.publication_snapshot.epoch(),
                base: OrganizationRevision::new(0),
                revision: self.publication_snapshot.state().revision,
            };
            if self.info.major == 1 {
                organization::v1::snapshot_payload(&self.publication_snapshot, range, part).unwrap()
            } else {
                organization::v2::snapshot_payload(&self.publication_snapshot, range, part).unwrap()
            }
        }
        #[track_caller]
        fn wait_connection_without_desktop(&self, ready: impl Fn(&ConnectionState) -> bool) {
            self.executor.block_on(async {
                let mut state = self.handle.state();
                tokio::time::timeout(std::time::Duration::from_secs(10), async {
                    loop {
                        if ready(&state.borrow_and_update()) {
                            break;
                        }
                        state.changed().await.unwrap();
                    }
                })
                .await
                .unwrap();
            });
        }
        fn wait_connected_without_desktop(&self) {
            self.wait_connection_without_desktop(
                |state| matches!(state, ConnectionState::Connected { info } if *info == self.published_info()),
            );
        }
        fn stage_without_desktop(&self) {
            self.executor.block_on(
                self.plan
                    .control(wire::peer::PeerControl::publication(self.part(0))),
            );
            self.wait_connection_without_desktop(|state| {
                matches!(
                    state,
                    ConnectionState::Connecting {
                        phase: spaceterm_client::api::ConnectingPhase::Reconciling,
                    }
                )
            });
        }
        fn stage(&self, cx: &mut TestAppContext) {
            self.stage_without_desktop();
            self.wait(cx, || {
                self.provider
                    .frame(self.pane.server())
                    .is_some_and(|frame| {
                        matches!(
                            frame.connection,
                            ConnectionState::Connecting {
                                phase: spaceterm_client::api::ConnectingPhase::Reconciling,
                            }
                        )
                    })
            });
            self.drive(cx);
        }
        fn commands(&self) -> Vec<Command> {
            self.plan
                .messages
                .lock()
                .unwrap()
                .iter()
                .filter_map(|payload| {
                    let request = match payload {
                        Payload::Major1(v1::Message {
                            kind: Some(v1::message::Kind::Operation(value)),
                        }) => operations::v1::decode_request(
                            value,
                            self.binding.target().pairing(),
                            self.binding.target().client(),
                            &spaceterm_core::operation_ledger::canonical::CanonicalCodec,
                        )
                        .unwrap(),
                        Payload::Major2(v2::Message {
                            kind: Some(v2::message::Kind::Operation(value)),
                        }) => operations::v2::decode_request(
                            value,
                            self.binding.target().pairing(),
                            self.binding.target().client(),
                            &spaceterm_core::operation_ledger::canonical::CanonicalCodec,
                        )
                        .unwrap(),
                        _ => return None,
                    };
                    Some(request.command().clone())
                })
                .collect()
        }
        fn shutdown(self, cx: &mut TestAppContext) {
            let task = cx.update(|cx| self.provider.prepare_quit(cx));
            finish(task, &self.executor, cx).unwrap();
            SNAPSHOTS
                .get()
                .unwrap()
                .lock()
                .unwrap()
                .remove(&self.pane.server());
        }
    }

    #[gpui::test]
    fn composed_authenticated_multipart_publication_preserves_original_startup_measurement(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::ui::init).unwrap();
        for major in [1, 2] {
            for coalesced in [false, true] {
                let fixture = Fixture::new(major, 50 + major as u8 + u8::from(coalesced) * 4, cx);
                let channel = fixture.provider.pane_channel(fixture.pane).unwrap();
                let terminal = fixture.terminal_pane(cx);
                fixture.start(cx);
                let original = fixture.provider.pane_measurement(fixture.pane).unwrap();
                // The real Pane emits once while authority is live. Delivery to its
                // desktop owner is delayed until the first publication part arrives.
                let measured = terminal.measure(original, cx);
                let commands = fixture.commands();
                let [Command::BeginStartup(intent)] = commands.as_slice() else {
                    panic!("deliberate use must accept exactly one startup intent: {commands:?}");
                };
                let intent = *intent;
                if coalesced {
                    fixture.stage_without_desktop();
                } else {
                    fixture.stage(cx);
                }
                assert_eq!(
                    fixture.provider.pane_measurement(fixture.pane),
                    Some(original)
                );
                assert_eq!(
                    fixture.provider.pane_view_token(fixture.pane).unwrap(),
                    original.token
                );
                assert_eq!(
                    fixture
                        .provider
                        .frame(fixture.pane.server())
                        .unwrap()
                        .organization
                        .as_ref(),
                    fixture.snapshot.as_ref()
                );
                for kind in [
                    EffectKind::Key,
                    EffectKind::Resize,
                    EffectKind::Read,
                    EffectKind::NativeRead,
                ] {
                    assert!(
                        channel.permit(kind).is_err(),
                        "partial publication cannot authorize {kind:?}"
                    );
                }
                let callback = cx
                    .update(|cx| {
                        fixture
                            .provider
                            .begin(DesktopRequest::Measured(measured), cx)
                    })
                    .unwrap();
                assert!(!callback.is_ready());
                assert!(matches!(
                    cx.update(|cx| fixture
                        .provider
                        .begin(DesktopRequest::Measured(measured), cx)),
                    Err(DesktopClientError::AdmissionFull)
                ));
                if !coalesced {
                    fixture.drive(cx);
                    assert!(!callback.is_ready());
                }
                assert_eq!(
                    fixture.commands(),
                    commands,
                    "staging cannot cancel or complete the accepted intent"
                );
                assert_eq!(
                    fixture.provider.pane_measurement(fixture.pane),
                    Some(original)
                );
                fixture.executor.block_on(
                    fixture
                        .plan
                        .control(wire::peer::PeerControl::publication(fixture.part(1))),
                );
                if coalesced {
                    // Finish both parts before the Pane can observe the intermediate
                    // watch value. It cannot supply a second measurement callback.
                    fixture.wait_connected_without_desktop();
                }
                assert!(finish(callback, &fixture.executor, cx).unwrap().is_some());
                fixture.wait(cx, || fixture.commands().len() == 2);
                let completed = fixture.commands();
                let [
                    Command::BeginStartup(original_intent),
                    Command::CompleteMeasuredStartup(measurement),
                ] = completed.as_slice()
                else {
                    panic!("the original click must complete exactly one startup: {completed:?}");
                };
                assert_eq!(*original_intent, intent);
                assert_eq!(measurement.intent, intent.id());
                assert_eq!(measurement.fence, intent.fence());
                assert_eq!(measurement.grid, measured.grid);
                fixture.wait(cx, || {
                    fixture
                        .provider
                        .frame(fixture.pane.server())
                        .is_some_and(|frame| {
                            frame.replica_status == ReplicaStatus::Ready
                                && matches!(frame.connection, ConnectionState::Connected { info } if info == fixture.published_info())
                        })
                });
                let frame = fixture.provider.frame(fixture.pane.server()).unwrap();
                let published = *channel.state().borrow();
                assert_eq!(published.connection, Some(fixture.published_info()));
                assert_eq!(
                    frame.organization.as_ref(),
                    fixture.publication_snapshot.as_ref()
                );
                assert_eq!(
                    published.runtime,
                    frame
                        .organization
                        .runtime()
                        .iter()
                        .find(|runtime| runtime.pane == fixture.pane)
                        .copied()
                );
                assert!(fixture.provider.pane_measurement(fixture.pane).is_none());
                assert!(matches!(
                    cx.update(|cx| fixture
                        .provider
                        .begin(DesktopRequest::Measured(measured), cx)),
                    Err(DesktopClientError::StaleContext)
                ));
                if coalesced {
                    terminal.terminal.update(cx, |pane, cx| {
                        pane.request_measurement(
                            original.token,
                            original.ticket,
                            original.layout,
                            cx,
                        )
                        .unwrap();
                    });
                }
                fixture.drive(cx);
                if coalesced {
                    assert_eq!(terminal.received.borrow().as_slice(), &[measured]);
                }
                assert_eq!(fixture.commands(), completed);
                drop(terminal);
                fixture.shutdown(cx);
            }
        }
    }

    fn revocation_case(major: u32, disconnect: bool, cx: &mut TestAppContext) {
        let fixture = Fixture::new(major, if disconnect { 60 } else { 70 } + major as u8, cx);
        let terminal = fixture.terminal_pane(cx);
        fixture.start(cx);
        let original = fixture.provider.pane_measurement(fixture.pane).unwrap();
        let measured = DesktopRequest::Measured(terminal.measure(original, cx));
        fixture.stage(cx);
        assert_eq!(
            fixture.provider.pane_measurement(fixture.pane),
            Some(original)
        );
        let callback = cx
            .update(|cx| fixture.provider.begin(measured.clone(), cx))
            .unwrap();
        fixture.drive(cx);
        assert!(!callback.is_ready());
        assert!(matches!(
            cx.update(|cx| fixture.provider.begin(measured.clone(), cx)),
            Err(DesktopClientError::AdmissionFull)
        ));
        let before = fixture.commands();
        assert!(matches!(before.as_slice(), [Command::BeginStartup(_)]));
        let control = if disconnect {
            wire::peer::PeerControl::Disconnect
        } else {
            // A delta cannot join a staged snapshot. This enters genuine
            // reconciliation, rather than completing the subscription transfer.
            wire::peer::PeerControl::publication(fixture.interleaved_delta())
        };
        fixture.executor.block_on(fixture.plan.control(control));
        fixture.wait(cx, || {
            fixture
                .provider
                .frame(fixture.pane.server())
                .is_some_and(|frame| matches!(frame.connection, ConnectionState::Connected { .. }))
                && fixture
                    .provider
                    .pane_view_token(fixture.pane)
                    .is_ok_and(|token| {
                        token.context_generation != original.token.context_generation
                    })
        });
        assert_eq!(
            finish(callback, &fixture.executor, cx),
            Err(DesktopClientError::StaleContext)
        );
        assert!(fixture.provider.pane_measurement(fixture.pane).is_none());
        assert!(matches!(
            cx.update(|cx| fixture.provider.begin(measured, cx)),
            Err(DesktopClientError::StaleContext)
        ));
        fixture.drive(cx);
        assert!(fixture.commands().iter().all(|command| !matches!(
            command,
            Command::CompleteMeasuredStartup(_) | Command::CompleteStartup(_)
        )));
        let current = match *fixture.handle.state().borrow() {
            ConnectionState::Connected { info } => info,
            ref other => panic!("revocation must settle on an authenticated connection: {other:?}"),
        };
        if disconnect {
            assert_ne!(current.binding, fixture.info.binding);
            assert_ne!(current.attempt, fixture.info.attempt);
        } else {
            assert_eq!(
                current, fixture.info,
                "same-connection reconciliation must still revoke the original use"
            );
        }
        assert_eq!(terminal.received.borrow().len(), 1);
        drop(terminal);
        fixture.shutdown(cx);
    }

    #[gpui::test]
    fn composed_authenticated_snapshot_delta_mixing_revokes_original_startup_measurement(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::ui::init).unwrap();
        for major in [1, 2] {
            revocation_case(major, false, cx);
        }
    }

    #[gpui::test]
    fn composed_authenticated_disconnect_revokes_original_startup_measurement(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::ui::init).unwrap();
        for major in [1, 2] {
            revocation_case(major, true, cx);
        }
    }

    #[gpui::test]
    fn composed_authenticated_selection_round_trip_cancels_deferred_measurement(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::ui::init).unwrap();
        for major in [1, 2] {
            let fixture = Fixture::build(major, 80 + major as u8, true, cx);
            let terminal = fixture.terminal_pane(cx);
            fixture.start(cx);
            let original = fixture.provider.pane_measurement(fixture.pane).unwrap();
            let measured = terminal.measure(original, cx);
            fixture.stage(cx);
            let callback = cx
                .update(|cx| {
                    fixture
                        .provider
                        .begin(DesktopRequest::Measured(measured), cx)
                })
                .unwrap();
            fixture.drive(cx);
            assert!(!callback.is_ready());
            let workspace = fixture.snapshot.state().workspaces[0].id;
            let original_tab = fixture.snapshot.state().panes[0].tab;
            let other_tab = fixture
                .snapshot
                .state()
                .tabs
                .iter()
                .find(|tab| tab.id != original_tab)
                .unwrap()
                .id;
            for tab in [other_tab, original_tab] {
                fixture.request(
                    DesktopRequest::PersonalEdit(ViewEdit::ActivateTab { workspace, tab }),
                    cx,
                );
            }
            assert_eq!(
                finish(callback, &fixture.executor, cx),
                Err(DesktopClientError::StaleContext)
            );
            assert!(fixture.provider.pane_measurement(fixture.pane).is_none());
            fixture.executor.block_on(
                fixture
                    .plan
                    .control(wire::peer::PeerControl::publication(fixture.part(1))),
            );
            fixture.wait(cx, || {
                fixture
                    .provider
                    .frame(fixture.pane.server())
                    .is_some_and(|frame| {
                        matches!(frame.connection, ConnectionState::Connected { .. })
                    })
            });
            fixture.drive(cx);
            assert!(fixture.commands().iter().all(|command| !matches!(
                command,
                Command::CompleteMeasuredStartup(_) | Command::CompleteStartup(_)
            )));
            // Cancellation releases callback admission and the old intent. Only a
            // new deliberate use, with its new ticket, can complete after publication.
            fixture.start(cx);
            let fresh = fixture.provider.pane_measurement(fixture.pane).unwrap();
            assert_ne!(fresh.ticket, original.ticket);
            let fresh_measurement = terminal.measure(fresh, cx);
            fixture.request(DesktopRequest::Measured(fresh_measurement), cx);
            fixture.wait(cx, || {
                fixture
                    .commands()
                    .iter()
                    .any(|command| matches!(command, Command::CompleteMeasuredStartup(_)))
            });
            let completed: Vec<_> = fixture
                .commands()
                .into_iter()
                .filter_map(|command| {
                    if let Command::CompleteMeasuredStartup(measurement) = command {
                        Some(measurement)
                    } else {
                        None
                    }
                })
                .collect();
            assert_eq!(completed.len(), 1);
            assert_eq!(completed[0].grid, fresh_measurement.grid);
            assert_eq!(
                terminal.received.borrow().as_slice(),
                &[measured, fresh_measurement]
            );
            assert!(fixture.provider.pane_measurement(fixture.pane).is_none());
            drop(terminal);
            fixture.shutdown(cx);
        }
    }
}

#[gpui::test]
fn composed_render_and_reconciliation_never_start_a_session(cx: &mut TestAppContext) {
    let harness = Harness::new(cx);
    for _ in 0..5 {
        let delivery = harness
            .provider
            .pane_presentation(harness.pane, None)
            .unwrap();
        assert_eq!(
            delivery.decision.presentation,
            view::PanePresentation::NotStarted
        );
        harness.publish(cx);
    }
    assert!(matches!(
        harness.server.snapshot().unwrap().runtime()[0].phase,
        spaceterm_core::organization::SessionPhase::NotStarted
    ));
    assert!(harness.provider.pane_measurement(harness.pane).is_none());
}

#[gpui::test]
fn composed_start_completes_the_original_intent_only_after_tagged_measurement(
    cx: &mut TestAppContext,
) {
    let harness = Harness::new(cx);
    harness.request(
        DesktopRequest::StartSession {
            pane: harness.pane,
            source: SelectionSource::Pointer,
        },
        cx,
    );
    harness.publish(cx);
    assert!(matches!(
        harness.server.snapshot().unwrap().runtime()[0].phase,
        spaceterm_core::organization::SessionPhase::NotStarted
    ));
    let request = harness.provider.pane_measurement(harness.pane).unwrap();
    let before_measurement = harness.server.received_operations().unwrap().len();
    harness.request(
        DesktopRequest::Measured(PaneMeasurement {
            token: request.token,
            ticket: request.ticket,
            layout: request.layout,
            grid: grid(),
        }),
        cx,
    );
    harness.publish(cx);
    assert!(matches!(
        harness.server.snapshot().unwrap().runtime()[0].phase,
        spaceterm_core::organization::SessionPhase::Running { .. }
    ));
    assert_eq!(
        harness.server.received_operations().unwrap().len(),
        before_measurement + 1
    );
    assert!(
        harness
            .provider
            .inner
            .state
            .borrow()
            .startup_channels
            .is_empty()
    );
    assert!(harness.provider.pane_measurement(harness.pane).is_none());
}

#[gpui::test]
fn composed_measurement_waits_for_reserved_view_save_and_is_accepted_once(cx: &mut TestAppContext) {
    let harness = Harness::new(cx);
    harness.request(
        DesktopRequest::StartSession {
            pane: harness.pane,
            source: SelectionSource::Pointer,
        },
        cx,
    );
    harness.publish(cx);
    let original = harness.provider.pane_measurement(harness.pane).unwrap();
    let measurement = DesktopRequest::Measured(PaneMeasurement {
        token: original.token,
        ticket: original.ticket,
        layout: original.layout,
        grid: grid(),
    });
    let before_measurement = harness.server.received_operations().unwrap().len();
    // Admission reserves the save lock before GPUI polls either task.
    let save = cx
        .update(|cx| {
            harness.provider.begin(
                DesktopRequest::PersonalEdit(ViewEdit::ActivateWorkspace(harness.workspace)),
                cx,
            )
        })
        .unwrap();
    let measured = cx
        .update(|cx| harness.provider.begin(measurement.clone(), cx))
        .unwrap();
    assert!(!measured.is_ready());
    assert!(matches!(
        cx.update(|cx| harness.provider.begin(measurement.clone(), cx)),
        Err(DesktopClientError::AdmissionFull)
    ));
    assert_eq!(
        harness.provider.pane_measurement(harness.pane),
        Some(original)
    );
    assert_eq!(
        harness.server.received_operations().unwrap().len(),
        before_measurement
    );
    finish(save, &harness.tokio, cx).unwrap();
    finish(measured, &harness.tokio, cx).unwrap();
    harness.publish(cx);
    assert!(matches!(
        harness.server.snapshot().unwrap().runtime()[0].phase,
        spaceterm_core::organization::SessionPhase::Running { .. }
    ));
    assert_eq!(
        harness.server.received_operations().unwrap().len(),
        before_measurement + 1
    );
    assert!(matches!(
        cx.update(|cx| harness.provider.begin(measurement, cx)),
        Err(DesktopClientError::StaleContext)
    ));
}

#[gpui::test]
fn composed_queued_measurement_never_substitutes_changed_context(cx: &mut TestAppContext) {
    let harness = Harness::new(cx);
    harness.request(
        DesktopRequest::StartSession {
            pane: harness.pane,
            source: SelectionSource::Pointer,
        },
        cx,
    );
    harness.publish(cx);
    let original = harness.provider.pane_measurement(harness.pane).unwrap();
    let save = cx
        .update(|cx| {
            harness.provider.begin(
                DesktopRequest::PersonalEdit(ViewEdit::ActivateWorkspace(harness.workspace)),
                cx,
            )
        })
        .unwrap();
    let measured = cx
        .update(|cx| {
            harness.provider.begin(
                DesktopRequest::Measured(PaneMeasurement {
                    token: original.token,
                    ticket: original.ticket,
                    layout: original.layout,
                    grid: grid(),
                }),
                cx,
            )
        })
        .unwrap();
    let before_measurement = harness.server.received_operations().unwrap().len();
    harness
        .provider
        .pane_channel(harness.pane)
        .unwrap()
        .update_context(spaceterm_client::pane_channel::PaneContextFacts::default())
        .unwrap();
    finish(save, &harness.tokio, cx).unwrap();
    assert_eq!(
        finish(measured, &harness.tokio, cx),
        Err(DesktopClientError::StaleContext)
    );
    assert_eq!(
        harness.server.received_operations().unwrap().len(),
        before_measurement
    );
    assert!(matches!(
        harness.server.snapshot().unwrap().runtime()[0].phase,
        spaceterm_core::organization::SessionPhase::NotStarted
    ));
}

#[gpui::test]
fn composed_queued_measurement_requires_new_geometry_after_saved_layout_change(
    cx: &mut TestAppContext,
) {
    let harness = Harness::new(cx);
    harness
        .tokio
        .block_on(harness.peer.submit(
            Command::CreatePane {
                tab: harness.tab,
                source: harness.pane,
                axis: spaceterm_core::organization::SplitAxis::Horizontal,
                edge: spaceterm_core::organization::SplitEdge::After,
            },
            Some(harness.workspace),
        ))
        .unwrap();
    harness.publish(cx);
    harness.request(
        DesktopRequest::StartSession {
            pane: harness.pane,
            source: SelectionSource::Pointer,
        },
        cx,
    );
    harness.publish(cx);
    let original = harness.provider.pane_measurement(harness.pane).unwrap();
    let save = cx
        .update(|cx| {
            harness.provider.begin(
                DesktopRequest::PersonalEdit(ViewEdit::Arrange {
                    workspace: harness.workspace,
                    tab: harness.tab,
                    edit: view::ArrangementEdit::ToggleZoom,
                }),
                cx,
            )
        })
        .unwrap();
    let measured = cx
        .update(|cx| {
            harness.provider.begin(
                DesktopRequest::Measured(PaneMeasurement {
                    token: original.token,
                    ticket: original.ticket,
                    layout: original.layout,
                    grid: grid(),
                }),
                cx,
            )
        })
        .unwrap();
    let before_measurement = harness.server.received_operations().unwrap().len();
    finish(save, &harness.tokio, cx).unwrap();
    assert_eq!(finish(measured, &harness.tokio, cx), Ok(None));
    let replacement = harness.provider.pane_measurement(harness.pane).unwrap();
    assert_eq!(replacement.token, original.token);
    assert_eq!(replacement.ticket, original.ticket);
    assert_ne!(replacement.layout, original.layout);
    assert_eq!(
        harness.server.received_operations().unwrap().len(),
        before_measurement
    );
    assert!(matches!(
        harness.server.snapshot().unwrap().runtime()[0].phase,
        spaceterm_core::organization::SessionPhase::NotStarted
    ));
    harness.request(
        DesktopRequest::Measured(PaneMeasurement {
            token: replacement.token,
            ticket: replacement.ticket,
            layout: replacement.layout,
            grid: grid(),
        }),
        cx,
    );
    harness.publish(cx);
    assert!(matches!(
        harness.server.snapshot().unwrap().runtime()[0].phase,
        spaceterm_core::organization::SessionPhase::Running { .. }
    ));
    assert_eq!(
        harness.server.received_operations().unwrap().len(),
        before_measurement + 1
    );
}

#[gpui::test]
fn composed_attach_then_start_needs_no_intervening_render(cx: &mut TestAppContext) {
    let harness = Harness::new(cx);
    harness
        .tokio
        .block_on(harness.peer.submit(
            Command::CreateWorkspace {
                server: harness.server.server(),
                source: WorkspaceCreationSource::ServerHome {
                    server: harness.server.server(),
                },
                name: NameDraft::Automatic,
            },
            None,
        ))
        .unwrap();
    harness.publish(cx);
    let snapshot = harness.server.snapshot().unwrap();
    let workspace = snapshot.state().workspaces[1].id;
    let pane = snapshot.state().panes[1].id;
    harness.request(DesktopRequest::AttachWorkspace(workspace), cx);
    let channel = harness.provider.pane_channel(pane).unwrap();
    assert_eq!(
        channel.state().borrow().facts,
        spaceterm_client::pane_channel::PaneContextFacts::default()
    );
    assert!(matches!(
        harness.server.snapshot().unwrap().runtime()[1].phase,
        spaceterm_core::organization::SessionPhase::NotStarted
    ));
    // No TerminalPane, presentation read or native render establishes focus for this action.
    harness.request(
        DesktopRequest::StartSession {
            pane,
            source: SelectionSource::Pointer,
        },
        cx,
    );
    harness.publish(cx);
    let request = harness.provider.pane_measurement(pane).unwrap();
    let facts = *channel.state().borrow();
    assert!(facts.facts.focused_pane && facts.facts.presentable);
    assert!(!facts.facts.terminal_input_focus);
    assert_eq!(request.token.context_generation, facts.context);
    harness.request(
        DesktopRequest::Measured(PaneMeasurement {
            token: request.token,
            ticket: request.ticket,
            layout: request.layout,
            grid: grid(),
        }),
        cx,
    );
    harness.publish(cx);
    assert!(matches!(
        harness.server.snapshot().unwrap().runtime()[1].phase,
        spaceterm_core::organization::SessionPhase::Running { .. }
    ));
    assert!(matches!(
        harness.server.snapshot().unwrap().runtime()[0].phase,
        spaceterm_core::organization::SessionPhase::NotStarted
    ));
    harness.request(
        DesktopRequest::SelectPane {
            pane: harness.pane,
            source: SelectionSource::Pointer,
        },
        cx,
    );
    let selected = harness.provider.pane_channel(harness.pane).unwrap();
    assert!(selected.state().borrow().facts.focused_pane);
    assert!(!channel.state().borrow().facts.focused_pane);
    assert_eq!(
        selected.state().borrow().context,
        channel.state().borrow().context
    );
}

#[gpui::test]
fn composed_stale_context_measurement_cannot_start_a_session(cx: &mut TestAppContext) {
    let harness = Harness::new(cx);
    harness.request(
        DesktopRequest::StartSession {
            pane: harness.pane,
            source: SelectionSource::Pointer,
        },
        cx,
    );
    harness.publish(cx);
    let request = harness.provider.pane_measurement(harness.pane).unwrap();
    harness
        .provider
        .pane_channel(harness.pane)
        .unwrap()
        .update_context(spaceterm_client::pane_channel::PaneContextFacts::default())
        .unwrap();
    let result = cx.update(|cx| {
        harness.provider.begin(
            DesktopRequest::Measured(PaneMeasurement {
                token: request.token,
                ticket: request.ticket,
                layout: request.layout,
                grid: grid(),
            }),
            cx,
        )
    });
    assert!(matches!(result, Err(DesktopClientError::StaleContext)));
    cx.update(|cx| harness.provider.revalidate_pane(harness.pane, cx));
    cx.run_until_parked();
    assert!(
        harness
            .provider
            .inner
            .state
            .borrow()
            .startup
            .get(&harness.server.server())
            .unwrap()
            .pending()
            .is_none()
    );
    assert!(matches!(
        harness.server.snapshot().unwrap().runtime()[0].phase,
        spaceterm_core::organization::SessionPhase::NotStarted
    ));
}

#[gpui::test]
fn composed_offline_navigation_does_not_queue_startup_or_shared_edits(cx: &mut TestAppContext) {
    let harness = Harness::new(cx);
    harness
        .provider
        .set_connection_block(
            harness.server.server(),
            Some((BlockedLocation::ThisComputer, IntendedAction::Connect)),
        )
        .unwrap();
    harness
        .provider
        .inner
        .channels
        .invalidate(
            harness.server.server(),
            spaceterm_client::ports::ConnectionInvalidation::ConnectionLost,
        )
        .unwrap();
    harness.connection.send_replace(ConnectionState::Stopped);
    cx.run_until_parked();
    harness.request(
        DesktopRequest::SelectPane {
            pane: harness.pane,
            source: SelectionSource::Command,
        },
        cx,
    );
    let delivery = harness
        .provider
        .pane_presentation(harness.pane, None)
        .unwrap();
    assert_eq!(
        delivery.cause,
        OfflineCause::Stopped(DisplayName::reported("Studio Mac"))
    );
    let request = DesktopRequest::Shared {
        server: harness.server.server(),
        command: Command::CreateWorkspace {
            server: harness.server.server(),
            source: WorkspaceCreationSource::ServerHome {
                server: harness.server.server(),
            },
            name: NameDraft::Automatic,
        },
    };
    assert!(matches!(
        cx.update(|cx| harness.provider.begin(request, cx)),
        Err(DesktopClientError::Offline)
    ));
    assert!(harness.provider.pane_measurement(harness.pane).is_none());
}

#[gpui::test]
fn composed_personal_edit_publishes_a_receipt_after_durable_view_save(cx: &mut TestAppContext) {
    let harness = Harness::new(cx);
    harness
        .tokio
        .block_on(harness.peer.submit(
            Command::CreatePane {
                tab: harness.tab,
                source: harness.pane,
                axis: spaceterm_core::organization::SplitAxis::Horizontal,
                edge: spaceterm_core::organization::SplitEdge::After,
            },
            Some(harness.workspace),
        ))
        .unwrap();
    harness.publish(cx);
    harness.request(
        DesktopRequest::PersonalEdit(ViewEdit::Arrange {
            workspace: harness.workspace,
            tab: harness.tab,
            edit: view::ArrangementEdit::ToggleZoom,
        }),
        cx,
    );
    assert!(
        matches!(harness.provider.frame(harness.server.server()).unwrap().personal.workspace(harness.workspace).unwrap().arrangement(harness.tab).unwrap().zoom(), view::ZoomState::Zoomed(pane) if pane == harness.pane)
    );
    assert_eq!(
        harness
            .provider
            .persistence
            .lock()
            .unwrap()
            .load()
            .unwrap()
            .unwrap(),
        harness
            .provider
            .inner
            .state
            .borrow()
            .view
            .persisted()
            .unwrap()
    );
}

#[gpui::test]
fn composed_native_factory_hosts_the_authenticated_channel_without_local_launch(
    cx: &mut TestAppContext,
) {
    use crate::ui::tab_view::ServerPaneFactory;
    cx.update(crate::ui::init).unwrap();
    let harness = Harness::new(cx);
    let construction = crate::ui::pane_lifecycle::PaneConstruction::testing();
    let visual = cx.add_empty_window();
    let terminal = visual.update(|window, cx| {
        ServerPaneFactory::create(&construction, harness.pane, &harness.runtime, window, cx)
            .unwrap()
    });
    visual.update(|_, cx| {
        assert_eq!(terminal.read(cx).server_pane_id(), Some(harness.pane));
    });
    visual.run_until_parked();
    assert!(matches!(
        harness.server.snapshot().unwrap().runtime()[0].phase,
        spaceterm_core::organization::SessionPhase::NotStarted
    ));
    drop(harness.tokio);
}

#[gpui::test]
fn composed_multi_server_views_keep_saved_order_and_disconnect_independently(
    cx: &mut TestAppContext,
) {
    let harness = Harness::build(cx, true);
    let (other, workspace, connection, _) = harness.other.as_ref().unwrap();
    assert_eq!(
        harness
            .provider
            .servers()
            .iter()
            .map(|info| info.server)
            .collect::<Vec<_>>(),
        vec![*other, harness.server.server()]
    );
    assert_eq!(
        harness.runtime.read_with(cx, |runtime, _| runtime
            .servers()
            .iter()
            .map(|info| info.server)
            .collect::<Vec<_>>()),
        vec![harness.server.server(), *other]
    );
    connection.send_replace(ConnectionState::Stopped);
    cx.run_until_parked();
    assert_eq!(
        harness.provider.frame(*other).unwrap().replica_status,
        ReplicaStatus::RetainedOffline
    );
    assert_eq!(
        harness
            .provider
            .frame(harness.server.server())
            .unwrap()
            .replica_status,
        ReplicaStatus::Ready
    );
    assert!(
        harness
            .provider
            .frame(*other)
            .unwrap()
            .personal
            .workspace(*workspace)
            .is_some()
    );
    assert!(
        harness
            .provider
            .frame(harness.server.server())
            .unwrap()
            .personal
            .workspace(harness.workspace)
            .is_some()
    );
}

#[gpui::test]
fn composed_shared_commands_use_the_owning_workspace_not_the_active_workspace(
    cx: &mut TestAppContext,
) {
    let harness = Harness::new(cx);
    harness
        .tokio
        .block_on(harness.peer.submit(
            Command::CreateWorkspace {
                server: harness.server.server(),
                source: WorkspaceCreationSource::ServerHome {
                    server: harness.server.server(),
                },
                name: NameDraft::Automatic,
            },
            None,
        ))
        .unwrap();
    harness.publish(cx);
    let other = harness.server.snapshot().unwrap().state().workspaces[1].id;
    harness.request(DesktopRequest::AttachWorkspace(other), cx);
    harness.request(
        DesktopRequest::PersonalEdit(ViewEdit::ActivateWorkspace(other)),
        cx,
    );
    let version = harness.server.snapshot().unwrap().state().workspaces[0]
        .name
        .version;
    for command in [
        Command::RenameWorkspace {
            workspace: harness.workspace,
            expected_version: version,
            draft: NameDraft::Custom(
                spaceterm_core::organization::NameText::new("Inactive workspace".into()).unwrap(),
            ),
        },
        Command::CreateTab {
            workspace: harness.workspace,
            source: spaceterm_core::operations::TabCreationSource::Pane(harness.pane),
        },
        Command::CreatePane {
            tab: harness.tab,
            source: harness.pane,
            axis: spaceterm_core::organization::SplitAxis::Horizontal,
            edge: spaceterm_core::organization::SplitEdge::After,
        },
    ] {
        let task = cx
            .update(|cx| {
                harness.provider.begin(
                    DesktopRequest::Shared {
                        server: harness.server.server(),
                        command,
                    },
                    cx,
                )
            })
            .unwrap();
        let operation = finish(task, &harness.tokio, cx).unwrap().unwrap();
        assert_eq!(
            harness
                .peer
                .journal()
                .snapshot()
                .unwrap()
                .operation(operation)
                .unwrap()
                .workspace,
            Some(harness.workspace),
        );
        harness.publish(cx);
    }
    let snapshot = harness.server.snapshot().unwrap();
    assert_eq!(
        snapshot
            .state()
            .workspaces
            .iter()
            .find(|workspace| workspace.id == harness.workspace)
            .unwrap()
            .name
            .value
            .display
            .as_str(),
        "Inactive workspace"
    );
    assert_eq!(
        snapshot
            .state()
            .tabs
            .iter()
            .filter(|tab| tab.workspace == harness.workspace)
            .count(),
        2
    );
    assert_eq!(
        snapshot
            .state()
            .panes
            .iter()
            .filter(|pane| pane.tab == harness.tab)
            .count(),
        2
    );
    assert_eq!(
        harness
            .provider
            .frame(harness.server.server())
            .unwrap()
            .personal
            .active_workspace(),
        Some(other)
    );
}

#[gpui::test]
fn composed_attachment_waits_for_the_authenticated_publication_cursor(cx: &mut TestAppContext) {
    let harness = Harness::new(cx);
    let old_info = match *harness.connection.borrow() {
        ConnectionState::Connected { info } => info,
        _ => panic!("connected fixture"),
    };
    harness
        .tokio
        .block_on(harness.peer.submit(
            Command::CreateWorkspace {
                server: harness.server.server(),
                source: WorkspaceCreationSource::ServerHome {
                    server: harness.server.server(),
                },
                name: NameDraft::Automatic,
            },
            None,
        ))
        .unwrap();
    harness.publish(cx);
    let workspace = harness.server.snapshot().unwrap().state().workspaces[1].id;
    harness
        .connection
        .send_replace(ConnectionState::Connected { info: old_info });
    cx.run_until_parked();
    let task = cx
        .update(|cx| {
            harness
                .provider
                .begin(DesktopRequest::AttachWorkspace(workspace), cx)
        })
        .unwrap();
    for _ in 0..3 {
        cx.run_until_parked();
        harness.tokio.block_on(tokio::task::yield_now());
    }
    assert!(
        !task.is_ready(),
        "attachment must wait rather than fail or expose an incoherent lease"
    );
    let ready = harness.server.connected_info(1, 1, 1, 2).unwrap();
    harness
        .connection
        .send_replace(ConnectionState::Connected { info: ready });
    finish(task, &harness.tokio, cx).unwrap();
    assert!(
        harness
            .provider
            .frame(harness.server.server())
            .unwrap()
            .personal
            .workspace(workspace)
            .is_some()
    );
    assert!(
        harness
            .server
            .snapshot()
            .unwrap()
            .runtime()
            .iter()
            .all(|runtime| matches!(
                runtime.phase,
                spaceterm_core::organization::SessionPhase::NotStarted
            ))
    );
}

#[gpui::test]
fn composed_attachment_is_journaled_and_detach_preserves_another_workspace(
    cx: &mut TestAppContext,
) {
    let harness = Harness::new(cx);
    harness
        .tokio
        .block_on(harness.peer.submit(
            Command::CreateWorkspace {
                server: harness.server.server(),
                source: WorkspaceCreationSource::ServerHome {
                    server: harness.server.server(),
                },
                name: NameDraft::Automatic,
            },
            None,
        ))
        .unwrap();
    harness.publish(cx);
    let snapshot = harness.server.snapshot().unwrap();
    let second_workspace = snapshot.state().workspaces[1].id;
    let second_pane = snapshot.state().panes[1].id;
    harness.request(DesktopRequest::AttachWorkspace(second_workspace), cx);
    let second_channel = harness.provider.pane_channel(second_pane).unwrap();
    let original_attachment = harness
        .provider
        .frame(harness.server.server())
        .unwrap()
        .personal
        .workspace(harness.workspace)
        .unwrap()
        .attachment();
    harness.request(DesktopRequest::DetachWorkspace(harness.workspace), cx);
    assert!(
        harness
            .provider
            .frame(harness.server.server())
            .unwrap()
            .personal
            .workspace(harness.workspace)
            .is_none()
    );
    assert!(
        harness
            .provider
            .frame(harness.server.server())
            .unwrap()
            .personal
            .workspace(second_workspace)
            .is_some()
    );
    assert_eq!(
        harness
            .provider
            .pane_channel(second_pane)
            .unwrap()
            .state()
            .borrow()
            .context,
        second_channel.state().borrow().context
    );
    assert_eq!(
        harness.server.snapshot().unwrap().state().workspaces.len(),
        2
    );
    assert!(view::AttachmentSuppression::suppresses(
        &harness.peer.journal().snapshot().unwrap(),
        &original_attachment,
    ));
}

#[gpui::test]
fn desktop_online_detach_drains_original_control_release(cx: &mut TestAppContext) {
    let harness = Harness::new(cx);
    harness.request(DesktopRequest::DetachWorkspace(harness.workspace), cx);
    assert!(matches!(
        harness.server.snapshot().unwrap().runtime()[0].control,
        spaceterm_core::organization::ControlRecord::Unclaimed
    ));
    assert!(
        harness
            .peer
            .journal()
            .snapshot()
            .unwrap()
            .detaches()
            .is_empty()
    );
}

#[gpui::test]
fn composed_journal_uncertainty_hides_frames_and_revokes_pane_delivery_synchronously(
    cx: &mut TestAppContext,
) {
    let harness = Harness::new(cx);
    harness.peer.journal().admission_latch().close();
    assert!(harness.provider.servers().is_empty());
    assert!(harness.provider.frame(harness.server.server()).is_none());
    assert!(matches!(
        harness.provider.pane_presentation(harness.pane, None),
        Err(DesktopClientError::LocalStorageUnavailable)
    ));
}

#[gpui::test]
fn composed_reconnect_keeps_the_screen_frozen_until_content_is_reconciled(cx: &mut TestAppContext) {
    use spaceterm_client::pane_channel::{EffectKind, PaneContextFacts};
    use spaceterm_core::{authority::*, content::*};
    let harness = Harness::new(cx);
    harness.request(
        DesktopRequest::StartSession {
            pane: harness.pane,
            source: SelectionSource::Pointer,
        },
        cx,
    );
    harness.publish(cx);
    let request = harness.provider.pane_measurement(harness.pane).unwrap();
    harness.request(
        DesktopRequest::Measured(PaneMeasurement {
            token: request.token,
            ticket: request.ticket,
            layout: request.layout,
            grid: grid(),
        }),
        cx,
    );
    harness.publish(cx);
    let channel = harness.provider.pane_channel(harness.pane).unwrap();
    channel
        .update_context(PaneContextFacts {
            focused_pane: true,
            terminal_input_focus: true,
            presentable: true,
        })
        .unwrap();
    let publish_base = |revision| {
        let state = *channel.state().borrow();
        let info = state.connection.unwrap();
        let runtime = state.runtime.unwrap();
        let spaceterm_core::organization::ControlRecord::Held { client, generation } =
            runtime.control
        else {
            panic!("the fixture lost control")
        };
        let session = SessionKey {
            epoch: info.epoch,
            pane: harness.pane,
            generation: runtime.session_generation,
        };
        let fence = EffectFence::new(
            session,
            ControlGrant::new(harness.pane, client, generation).unwrap(),
            info.binding,
            state.context,
            None,
        )
        .unwrap();
        channel
            .publish_content_base(
                fence,
                ContentBase {
                    session,
                    revision: ContentRevision::new(revision),
                    screen: Screen::Primary,
                    screen_generation: 1,
                    topology_revision: 1,
                },
            )
            .unwrap();
        session
    };
    let session = publish_base(1);
    assert!(
        harness
            .provider
            .pane_presentation(harness.pane, Some(session))
            .unwrap()
            .decision
            .eligibility
            .input
    );
    let old = channel.permit(EffectKind::Key).unwrap();
    harness
        .provider
        .inner
        .channels
        .invalidate(
            harness.server.server(),
            spaceterm_client::ports::ConnectionInvalidation::ConnectionLost,
        )
        .unwrap();
    harness.connection.send_replace(ConnectionState::Offline {
        failure: ClientFailure::new(ClientFailureClass::Unavailable),
        retry: spaceterm_client::api::RetryState {
            failures: 1,
            delay: None,
        },
    });
    cx.run_until_parked();
    let frozen = harness
        .provider
        .pane_presentation(harness.pane, Some(session))
        .unwrap();
    assert_eq!(
        frozen.decision.presentation,
        view::PanePresentation::Offline {
            prior: view::OfflinePrior::Frozen
        }
    );
    assert!(!frozen.decision.eligibility.input);
    assert!(!old.is_current());
    harness.peer.reconnect(2, 2).unwrap();
    harness.replica.send_replace(ReplicaState::Ready {
        snapshot: harness.server.replica().unwrap(),
    });
    harness.connection.send_replace(ConnectionState::Connected {
        info: harness.server.connected_info(1, 2, 2, 2).unwrap(),
    });
    cx.run_until_parked();
    let reconciling = harness
        .provider
        .pane_presentation(harness.pane, Some(session))
        .unwrap();
    assert!(matches!(
        reconciling.decision.presentation,
        view::PanePresentation::Reconciling { .. }
    ));
    assert!(!reconciling.decision.eligibility.input);
    assert!(channel.permit(EffectKind::Key).is_err());
    publish_base(2);
    assert!(
        harness
            .provider
            .pane_presentation(harness.pane, Some(session))
            .unwrap()
            .decision
            .eligibility
            .input
    );
    assert!(channel.permit(EffectKind::Key).is_ok());
    assert!(!old.is_current());
}

#[gpui::test]
fn composed_personal_transaction_is_reserved_before_any_task_is_polled(cx: &mut TestAppContext) {
    let harness = Harness::new(cx);
    let request = DesktopRequest::PersonalEdit(ViewEdit::ActivateWorkspace(harness.workspace));
    let first = cx
        .update(|cx| harness.provider.begin(request.clone(), cx))
        .unwrap();
    assert!(matches!(
        cx.update(|cx| harness.provider.begin(request.clone(), cx)),
        Err(DesktopClientError::AdmissionFull)
    ));
    finish(first, &harness.tokio, cx).unwrap();
    let next = cx.update(|cx| harness.provider.begin(request, cx)).unwrap();
    finish(next, &harness.tokio, cx).unwrap();
}

#[gpui::test]
fn composed_directory_repair_recovers_the_canonical_journal_operation(cx: &mut TestAppContext) {
    let h = Harness::new(cx);
    let command = Command::RepairPaneDirectory {
        pane: h.pane,
        expected_version: spaceterm_core::authority::FieldVersion::new(1),
        value: spaceterm_core::organization::ServerDirectory::new(
            h.pane.server(),
            b"/replacement".to_vec(),
        )
        .unwrap(),
    };
    let binding = h
        .provider
        .inner
        .source(&h.pane.server())
        .unwrap()
        .binding
        .clone();
    let original = h
        .tokio
        .block_on(h.provider.inner.tracker.submit(
            &binding,
            command.clone(),
            None,
            h.server.as_ref(),
        ))
        .unwrap();
    let recovered = cx
        .update(|cx| h.provider.retained_pane_repair(h.pane, cx))
        .unwrap();
    assert_eq!(recovered, Some((original.id(), command.clone())));
    h.tokio
        .block_on(h.provider.inner.tracker.resolve(
            original.id(),
            spaceterm_core::operations::OperationLookup::Completed {
                result: Outcome::Applied,
                publication_revision: OrganizationRevision::new(5),
            },
            h.server.as_ref(),
        ))
        .unwrap();
    // A durable outcome is not evidence that its directory has reached the replica.
    assert_eq!(
        cx.update(|cx| h.provider.retained_pane_repair(h.pane, cx))
            .unwrap(),
        Some((original.id(), command))
    );
}

#[gpui::test]
fn composed_admitted_directory_repair_blocks_replacement_before_task_poll(cx: &mut TestAppContext) {
    let h = Harness::new(cx);
    let request = DesktopRequest::Shared {
        server: h.pane.server(),
        command: Command::RepairPaneDirectory {
            pane: h.pane,
            expected_version: spaceterm_core::authority::FieldVersion::new(1),
            value: spaceterm_core::organization::ServerDirectory::new(
                h.pane.server(),
                b"/replacement".to_vec(),
            )
            .unwrap(),
        },
    };
    let first = cx
        .update(|cx| h.provider.begin(request.clone(), cx))
        .unwrap();
    let replacement = cx.update(|cx| h.provider.begin(request.clone(), cx));
    assert!(matches!(
        replacement,
        Err(DesktopClientError::AdmissionFull)
    ));
    assert!(matches!(
        cx.update(|cx| h.provider.retained_pane_repair(h.pane, cx)),
        Err(DesktopClientError::AdmissionFull)
    ));
    drop(first);
    cx.run_until_parked();
    assert!(cx.update(|cx| h.provider.begin(request, cx)).is_ok());
}

#[gpui::test]
fn desktop_offline_detach_reconnect_drains_original_release(cx: &mut gpui::TestAppContext) {
    let harness = Harness::new(cx);
    let retained = harness.server.replica().unwrap();
    harness.connection.send_replace(ConnectionState::Offline {
        failure: ClientFailure::new(ClientFailureClass::Unavailable),
        retry: spaceterm_client::api::RetryState {
            failures: 1,
            delay: None,
        },
    });
    harness.replica.send_replace(ReplicaState::Stale {
        retained,
        reason: spaceterm_client::replica::ResyncReason::ConnectionLost,
    });
    cx.run_until_parked();
    harness.request(DesktopRequest::DetachWorkspace(harness.workspace), cx);
    assert_eq!(
        harness.peer.journal().snapshot().unwrap().detaches().len(),
        1
    );
    assert!(matches!(
        harness.server.snapshot().unwrap().runtime()[0].control,
        spaceterm_core::organization::ControlRecord::Held { .. }
    ));
    harness.publish(cx);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !harness
        .peer
        .journal()
        .snapshot()
        .unwrap()
        .detaches()
        .is_empty()
    {
        assert!(
            std::time::Instant::now() < deadline,
            "authenticated reconnect did not drain the original task"
        );
        harness.tokio.block_on(tokio::task::yield_now());
        cx.run_until_parked();
    }
    assert!(matches!(
        harness.server.snapshot().unwrap().runtime()[0].control,
        spaceterm_core::organization::ControlRecord::Unclaimed
    ));
}

#[gpui::test]
fn composed_repeat_selection_preserves_the_original_pending_measurement(cx: &mut TestAppContext) {
    let harness = Harness::new(cx);
    harness.request(
        DesktopRequest::StartSession {
            pane: harness.pane,
            source: SelectionSource::Pointer,
        },
        cx,
    );
    harness.publish(cx);
    let original = harness.provider.pane_measurement(harness.pane).unwrap();
    let operations = harness.server.received_operations().unwrap().len();
    harness.request(
        DesktopRequest::SelectPane {
            pane: harness.pane,
            source: SelectionSource::Pointer,
        },
        cx,
    );
    harness.publish(cx);
    let repeated = harness.provider.pane_measurement(harness.pane).unwrap();
    assert_eq!(repeated.token, original.token);
    assert_eq!(repeated.ticket, original.ticket);
    assert_eq!(repeated.layout, original.layout);
    assert_eq!(
        harness.server.received_operations().unwrap().len(),
        operations
    );
    harness.request(
        DesktopRequest::Measured(PaneMeasurement {
            token: original.token,
            ticket: original.ticket,
            layout: original.layout,
            grid: grid(),
        }),
        cx,
    );
    harness.publish(cx);
    assert!(matches!(
        harness.server.snapshot().unwrap().runtime()[0].phase,
        spaceterm_core::organization::SessionPhase::Running { .. }
    ));
}
