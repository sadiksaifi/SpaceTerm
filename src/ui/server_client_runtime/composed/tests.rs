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
            sources,
            order,
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
    let binding = h.provider.inner.sources[&h.pane.server()].binding.clone();
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
