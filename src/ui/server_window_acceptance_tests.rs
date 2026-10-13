//! Desktop acceptance with real account servers, paired IPC, native journals and view files.
//! Only service supervision, account selection and test signing access are injected.

use super::*;
use crate::platform::{
    app_paths::AppPaths,
    client_view_store::{NativeViewStore, paths_for_root},
    desktop_client_startup::{self, AcceptanceDecoder, AcceptanceProtocol, NativeDesktopOwners},
};
use crate::ui::{
    server_client_runtime::{
        DesktopClientDependencies, DesktopClientProvider, DesktopClientRuntime,
        composed::ComposedDesktopClientProvider, notification_channel,
    },
    server_desktop_contracts::{DesktopRequest, DesktopServerFrame, ViewEdit},
};
use gpui::{TestAppContext, VisualTestContext};
use spaceterm_client::{
    api::{ConnectionState, ConnectionTarget, PairedServerBinding},
    journal::{
        ClientJournalChange, JournalController, JournalLimits, ReceiptVerifier, SavedBinding,
    },
    operations::{OperationStatus, OperationTracker},
    pane_channel::PaneChannels,
    ports::{ClientFuture, ConnectionPurpose, CredentialSource, SigningAccess},
    projection::ProjectionBudgets,
    view::{ArrangementEdit, ArrangementMetrics, PaneNodeRef, PaneSize, ViewPersistence},
};
use spaceterm_client_native::{
    journal::NativeClientJournal,
    route::{LocalRouteAuthority, NativeRouteConnector},
};
use spaceterm_core::{
    identity::{ServerId, TabId, WorkspaceId},
    operations::{Command, Outcome, TabCreationSource, WorkspaceCreationSource},
    organization::{NameDraft, SessionPhase, SplitAxis, SplitEdge},
    pairing::{CredentialReference, LocalPairingGeneration, SignedPairingReceipt},
};
use spaceterm_protocol::{authentication::ServerPin, pairing_adapter::ReceiptCodec};
use std::time::Duration;

// Read-only reuse of the route lane's production composition, not a second server implementation.
#[path = "../../crates/spaceterm-server/tests/support/authenticated_routes.rs"]
mod authenticated;

struct Verifier;
impl ReceiptVerifier for Verifier {
    fn verify(
        &self,
        pin: ServerPin,
        receipt: &SignedPairingReceipt,
    ) -> Result<(), spaceterm_client::failure::ClientFailure> {
        ReceiptCodec::new(pin)
            .verify(receipt)
            .map(|_| ())
            .map_err(|_| {
                spaceterm_client::failure::ClientFailure::new(
                    spaceterm_client::failure::ClientFailureClass::InvalidPeer,
                )
            })
    }
}
struct RecordingMovement;
impl crate::platform::window_movement::WindowMovementFactory for RecordingMovement {
    fn create(
        &self,
    ) -> Rc<dyn crate::platform::window_movement::OperatingSystemWindowDragPlatform> {
        Rc::new(
            crate::platform::window_movement::RecordingOperatingSystemWindowDragPlatform::default(),
        )
    }
}
struct TestSigning;
impl CredentialSource for TestSigning {
    fn signing_access<'a>(
        &'a self,
        target: &'a ConnectionTarget,
        _: ConnectionPurpose,
    ) -> ClientFuture<'a, SigningAccess> {
        Box::pin(async move {
            Ok(SigningAccess(Arc::new(authenticated::Key::new(
                target.device().bytes()[0] + 1,
            ))))
        })
    }
}

struct Desktop {
    _root: tempfile::TempDir,
    paths: Arc<AppPaths>,
    journal_path: PathBuf,
    journal: Arc<JournalController>,
    registry: spaceterm_client::registry::ClientRegistry,
    tracker: OperationTracker,
    protocol: Arc<AcceptanceProtocol>,
    bindings: Vec<PairedServerBinding>,
    provider: Rc<ComposedDesktopClientProvider<NativeViewStore>>,
    runtime: Entity<DesktopClientRuntime>,
    manager: Entity<WorkspaceManager>,
    window: gpui::AnyWindowHandle,
}
impl Desktop {
    fn new(
        executor: &tokio::runtime::Runtime,
        servers: &[&authenticated::Fixture],
        client: usize,
        cx: &mut TestAppContext,
    ) -> Self {
        let root = tempfile::tempdir_in(servers[0]._root.path().parent().unwrap()).unwrap();
        let paths = paths_for_root(root.path());
        let journal_path = root.path().join("journal");
        paths
            .filesystem()
            .ensure_durable_private_directory(&journal_path)
            .unwrap();
        let (composition, registry, journal, tracker, protocol, bindings) =
            executor.block_on(async {
                let codec = Arc::new(spaceterm_core::operation_ledger::canonical::CanonicalCodec);
                let native = Arc::new(
                    NativeClientJournal::create(
                        journal_path.clone(),
                        codec.clone(),
                        Arc::new(Verifier),
                        JournalLimits::default(),
                    )
                    .await
                    .unwrap(),
                );
                let journal = Arc::new(
                    JournalController::recover(
                        native,
                        codec,
                        Arc::new(Verifier),
                        JournalLimits::default(),
                    )
                    .await
                    .unwrap(),
                );
                let mut bindings = Vec::new();
                let mut routes = Vec::new();
                for (index, server) in servers.iter().enumerate() {
                    let route = LocalRouteAuthority::new(server.endpoint.clone()).unwrap();
                    let streams = server.enrollment[client].clone();
                    let ids = streams.parts().client;
                    let binding = PairedServerBinding::new(
                        ConnectionTarget::new(
                            server.pin,
                            spaceterm_core::identity::DeviceId::new(
                                server.principals[client].device,
                            ),
                            ids.pairing,
                            ids.id,
                            route.description().clone(),
                            CredentialReference::new([client as u8 + 1; 16]),
                            LocalPairingGeneration::new(index as u64 + 1).unwrap(),
                        )
                        .unwrap(),
                    );
                    journal
                        .transact(vec![ClientJournalChange::SaveBinding(Box::new(
                            SavedBinding::verified(
                                binding.clone(),
                                streams,
                                server.receipts[client].clone(),
                                &Verifier,
                            )
                            .unwrap(),
                        ))])
                        .await
                        .unwrap();
                    bindings.push(binding);
                    routes.push(route);
                }
                let tracker = OperationTracker::new(journal.clone());
                let protocol = Arc::new(AcceptanceProtocol::new(journal.clone(), tracker.clone()));
                let channels = Arc::new(
                    PaneChannels::new(protocol.clone(), ProjectionBudgets::supported().unwrap())
                        .unwrap(),
                );
                protocol.set_channels(channels.clone()).unwrap();
                let composition = desktop_client_startup::recover(
                    NativeDesktopOwners {
                        services: spaceterm_client::ports::ClientServices {
                            routes: Arc::new(NativeRouteConnector::new(routes, None).unwrap()),
                            credentials: Arc::new(TestSigning),
                            journal: journal.clone(),
                            organization: Arc::new(AcceptanceDecoder),
                            delegate: protocol.clone(),
                            discovery: None,
                            limits: Default::default(),
                            additional_capabilities: vec![],
                        },
                        journal: journal.clone(),
                        channels,
                        operations: protocol.clone(),
                        invalidator: protocol.clone(),
                        local_server: None,
                        tracker: Some(tracker.clone()),
                        registry_binding: Some(protocol.clone()),
                    },
                    NativeViewStore::new(paths.clone()),
                )
                .await
                .unwrap();
                let registry = composition.registry.clone();
                (composition, registry, journal, tracker, protocol, bindings)
            });
        let (notifier, notifications) = notification_channel();
        let provider = cx.update(|cx| {
            Rc::new(ComposedDesktopClientProvider::new(composition, notifier, cx).unwrap())
        });
        let runtime = cx.new(|cx| {
            DesktopClientRuntime::new(
                DesktopClientDependencies {
                    provider: provider.clone(),
                    notifications,
                },
                cx,
            )
        });
        let (manager, visual) = cx.add_window_view(|window, cx| {
            WorkspaceManager::new_server(
                runtime.clone(),
                super::tests::workspace_adapters(super::tests::test_remote_backend_factory()),
                SidebarState::Expanded,
                window,
                cx,
            )
        });
        let window = visual.update(|window, _| window.window_handle());
        executor.block_on(async {
            for binding in &bindings {
                registry
                    .server(binding.target().server())
                    .unwrap()
                    .connect()
                    .await
                    .unwrap();
            }
        });
        Self {
            _root: root,
            paths,
            journal_path,
            journal,
            registry,
            tracker,
            protocol,
            bindings,
            provider,
            runtime,
            manager,
            window,
        }
    }
    fn wait_connected(
        &self,
        server: ServerId,
        executor: &tokio::runtime::Runtime,
        cx: &mut TestAppContext,
    ) {
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        let handle = self.registry.server(server).unwrap();
        while !self
            .provider
            .frame(server)
            .is_some_and(|frame| matches!(frame.connection, ConnectionState::Connected { .. }))
        {
            assert!(
                std::time::Instant::now() < deadline,
                "connection did not settle: {:?}; desktop: {:?}",
                *handle.state().borrow(),
                self.provider
                    .frame(server)
                    .map(|frame| frame.connection.clone())
            );
            drive(executor, cx);
        }
    }
    fn wait_workspaces(
        &self,
        server: ServerId,
        count: usize,
        executor: &tokio::runtime::Runtime,
        cx: &mut TestAppContext,
    ) {
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        let handle = self.registry.server(server).unwrap();
        while self.frame(server).organization.state().workspaces.len() != count {
            assert!(
                std::time::Instant::now() < deadline,
                "workspace publication did not settle: connection={:?}, replica={:?}, desktop_revision={:?}, desktop_count={}",
                *handle.state().borrow(),
                handle
                    .replica()
                    .borrow()
                    .retained()
                    .map(|snapshot| snapshot.cursor),
                self.frame(server).organization.state().revision,
                self.frame(server).organization.state().workspaces.len()
            );
            drive(executor, cx);
        }
    }
    fn frame(&self, server: ServerId) -> Arc<DesktopServerFrame> {
        self.provider.frame(server).unwrap()
    }
    fn request(
        &self,
        request: DesktopRequest,
        executor: &tokio::runtime::Runtime,
        cx: &mut TestAppContext,
    ) {
        let task = cx.update(|cx| self.provider.begin(request, cx).unwrap());
        settle_until(executor, cx, || task.is_ready());
        cx.foreground_executor().block_test(task).unwrap();
        drive(executor, cx);
    }
    fn shared(&self, command: Command, executor: &tokio::runtime::Runtime) -> Outcome {
        executor.block_on(async {
            let binding = self
                .bindings
                .iter()
                .find(|binding| binding.target().server() == command.server())
                .unwrap();
            let ticket = self
                .tracker
                .submit(binding, command, None, self.protocol.as_ref())
                .await
                .unwrap();
            let mut status = ticket.status();
            tokio::time::timeout(Duration::from_secs(20), async {
                loop {
                    let current = status.borrow().clone();
                    if let OperationStatus::Completed { outcome, .. } = current {
                        return outcome;
                    }
                    status.changed().await.unwrap();
                }
            })
            .await
            .unwrap()
        })
    }
    fn workspace_entity(&self, workspace: WorkspaceId, cx: &TestAppContext) -> Entity<TabManager> {
        self.manager.read_with(cx, |manager, _| {
            manager
                .server
                .as_ref()
                .unwrap()
                .collection
                .payload(workspace)
                .unwrap()
                .clone()
        })
    }
}
fn drive(executor: &tokio::runtime::Runtime, cx: &mut TestAppContext) {
    for _ in 0..15 {
        cx.run_until_parked();
        executor.block_on(async { tokio::time::sleep(Duration::from_millis(2)).await });
    }
    cx.run_until_parked();
}
#[track_caller]
fn settle_until(
    executor: &tokio::runtime::Runtime,
    cx: &mut TestAppContext,
    ready: impl Fn() -> bool,
) {
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while !ready() {
        assert!(
            std::time::Instant::now() < deadline,
            "authenticated desktop publication did not settle"
        );
        drive(executor, cx);
    }
}
fn create_workspace(
    desktop: &Desktop,
    server: ServerId,
    executor: &tokio::runtime::Runtime,
) -> (WorkspaceId, TabId, spaceterm_core::identity::PaneId) {
    let outcome = desktop.shared(
        Command::CreateWorkspace {
            server,
            source: WorkspaceCreationSource::ServerHome { server },
            name: NameDraft::Automatic,
        },
        executor,
    );
    let Outcome::CreatedWithControl(created) = outcome else {
        panic!("workspace creation: {outcome:?}")
    };
    let spaceterm_core::operations::CreatedEntities::Workspace {
        workspace,
        tab,
        pane,
    } = created.entities()
    else {
        panic!("workspace identities")
    };
    (workspace, tab, pane)
}

fn compatible_server(executor: &tokio::runtime::Runtime, major: u32) -> authenticated::Fixture {
    executor.block_on(async {
        let server = authenticated::Fixture::with_bootstrap_client(major).await;
        let mut routes = authenticated::Routes {
            fixture: &server,
            route: authenticated::Route::Ipc,
            wss: None,
            ssh: None,
            next_endpoint: 0,
        };
        let route = routes.open().await;
        let (stream, principal) = server.bootstrap.unwrap();
        let codec = authenticated::handshake_limits(
            &route,
            server.pin,
            principal,
            major,
            47,
            Default::default(),
        )
        .await
        .unwrap();
        let mut bootstrap = authenticated::Client {
            route,
            codec,
            stream,
            sequence: 1,
            last_frame: vec![],
            generation: None,
            runtime_request: 0,
            rows: BTreeMap::new(),
            text: String::new(),
        };
        bootstrap.reconcile(&server.handle).await;
        // Exercise established compatible servers. The revision-zero native grant
        // reconciliation defect is reported separately rather than bypassed in production.
        assert_eq!(
            bootstrap
                .command(Command::SetWorktreeLocation {
                    server: stream.server(),
                    expected_version: server
                        .handle
                        .snapshot()
                        .state()
                        .settings
                        .worktree_location
                        .version,
                    template: spaceterm_core::organization::WorktreeLocationTemplate::new(
                        "~/.worktrees/{repository}/acceptance-{branch}".into()
                    )
                    .unwrap(),
                })
                .await,
            Outcome::Applied
        );
        bootstrap.route.close().await;
        drop(routes);
        server
    })
}

#[gpui::test]
fn authenticated_desktop_two_servers_preserve_independent_views_and_inactive_entities(
    cx: &mut TestAppContext,
) {
    cx.update(crate::ui::init).unwrap();
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let first = compatible_server(&executor, 2);
    let second = compatible_server(&executor, 1);
    let a = Desktop::new(&executor, &[&first, &second], 0, cx);
    let b = Desktop::new(&executor, &[&first, &second], 1, cx);
    let first_id = first.handle.snapshot().state().server;
    let second_id = second.handle.snapshot().state().server;
    for desktop in [&a, &b] {
        for server in [first_id, second_id] {
            desktop.wait_connected(server, &executor, cx);
        }
    }
    let (workspace, tab, pane) = create_workspace(&a, first_id, &executor);
    let (other, _, _) = create_workspace(&a, second_id, &executor);
    b.wait_workspaces(first_id, 1, &executor, cx);
    b.wait_workspaces(second_id, 1, &executor, cx);
    for desktop in [&a, &b] {
        for target in [workspace, other] {
            desktop.request(DesktopRequest::AttachWorkspace(target), &executor, cx);
        }
    }
    a.request(
        DesktopRequest::PersonalEdit(ViewEdit::ActivateWorkspace(workspace)),
        &executor,
        cx,
    );
    b.request(
        DesktopRequest::PersonalEdit(ViewEdit::ActivateWorkspace(other)),
        &executor,
        cx,
    );
    let a_entity = a.workspace_entity(workspace, cx);
    let b_entity = b.workspace_entity(workspace, cx);
    let a_tab_entity = a_entity.read_with(cx, |tabs, _| tabs.server_tab(tab).unwrap());
    let b_tab_entity = b_entity.read_with(cx, |tabs, _| tabs.server_tab(tab).unwrap());
    let outcome = a.shared(
        Command::CreateTab {
            workspace,
            source: TabCreationSource::Pane(pane),
        },
        &executor,
    );
    let Outcome::CreatedWithControl(created) = outcome else {
        panic!("tab creation: {outcome:?}")
    };
    let spaceterm_core::operations::CreatedEntities::Tab { tab: new_tab, .. } = created.entities()
    else {
        panic!("tab identities")
    };
    settle_until(&executor, cx, || {
        b.frame(first_id)
            .personal
            .workspace(workspace)
            .unwrap()
            .tab_order()
            .len()
            == 2
    });
    assert_eq!(b.frame(first_id).personal.active_workspace(), Some(other));
    assert_eq!(
        b_entity.read_with(cx, |tabs, _| tabs.server_tab_order()),
        vec![tab, new_tab]
    );
    assert_eq!(
        a.workspace_entity(workspace, cx).entity_id(),
        a_entity.entity_id()
    );
    assert_eq!(
        b.workspace_entity(workspace, cx).entity_id(),
        b_entity.entity_id()
    );
    assert_eq!(
        a_entity.read_with(cx, |tabs, _| tabs.server_tab(tab).unwrap().entity_id()),
        a_tab_entity.entity_id()
    );
    assert_eq!(
        b_entity.read_with(cx, |tabs, _| tabs.server_tab(tab).unwrap().entity_id()),
        b_tab_entity.entity_id()
    );
    let split = a.shared(
        Command::CreatePane {
            tab,
            source: pane,
            axis: SplitAxis::Horizontal,
            edge: SplitEdge::After,
        },
        &executor,
    );
    let Outcome::CreatedWithControl(created) = split else {
        panic!("split creation: {split:?}")
    };
    let new_pane = created.entities().pane();
    settle_until(&executor, cx, || {
        b.frame(first_id)
            .personal
            .workspace(workspace)
            .unwrap()
            .arrangement(tab)
            .unwrap()
            .pane_count()
            == 2
    });
    let b_arrangement = b
        .frame(first_id)
        .personal
        .workspace(workspace)
        .unwrap()
        .arrangement(tab)
        .unwrap()
        .clone();
    let split = match a
        .frame(first_id)
        .personal
        .workspace(workspace)
        .unwrap()
        .arrangement(tab)
        .unwrap()
        .tree()
        .node()
    {
        PaneNodeRef::Split { id, .. } => id,
        PaneNodeRef::Pane(_) => panic!("shared split must create a personal layout"),
    };
    let available = PaneSize::new(1200.0, 800.0).unwrap();
    let metrics = ArrangementMetrics {
        minimum_leaf: PaneSize::new(80.0, 60.0).unwrap(),
        divider: 1.0,
    };
    for edit in [
        ArrangementEdit::Focus(new_pane),
        ArrangementEdit::Focus(pane),
        ArrangementEdit::FocusPrevious,
        ArrangementEdit::ToggleZoom,
        ArrangementEdit::SetSplitAxis {
            split,
            axis: SplitAxis::Vertical,
            available,
            metrics,
        },
        ArrangementEdit::Resize {
            split,
            available,
            metrics,
            ratio: 0.7,
        },
    ] {
        a.request(
            DesktopRequest::PersonalEdit(ViewEdit::Arrange {
                workspace,
                tab,
                edit,
            }),
            &executor,
            cx,
        );
    }
    let a_arrangement = a
        .frame(first_id)
        .personal
        .workspace(workspace)
        .unwrap()
        .arrangement(tab)
        .unwrap()
        .clone();
    assert_eq!(a_arrangement.focused_pane(), Some(new_pane));
    assert_eq!(
        a_arrangement.zoom(),
        spaceterm_client::view::ZoomState::Zoomed(new_pane)
    );
    assert!(a_arrangement.focus_history().contains(&pane));
    assert!(
        matches!(a_arrangement.tree().node(), PaneNodeRef::Split { axis: SplitAxis::Vertical, ratio, .. } if (ratio - 0.7).abs() < 0.001)
    );
    assert_eq!(
        b.frame(first_id)
            .personal
            .workspace(workspace)
            .unwrap()
            .arrangement(tab)
            .unwrap(),
        &b_arrangement
    );
    let shared_revision = first.handle.snapshot().state().revision;
    a.request(
        DesktopRequest::PersonalEdit(ViewEdit::MoveTab {
            workspace,
            tab: new_tab,
            position: 0,
        }),
        &executor,
        cx,
    );
    a.request(
        DesktopRequest::PersonalEdit(ViewEdit::ActivateTab {
            workspace,
            tab: new_tab,
        }),
        &executor,
        cx,
    );
    assert_eq!(
        a.frame(first_id)
            .personal
            .workspace(workspace)
            .unwrap()
            .tab_order(),
        &[new_tab, tab]
    );
    assert_eq!(
        b.frame(first_id)
            .personal
            .workspace(workspace)
            .unwrap()
            .tab_order(),
        &[tab, new_tab]
    );
    assert_eq!(
        b.frame(first_id)
            .personal
            .workspace(workspace)
            .unwrap()
            .active_tab(),
        Some(tab)
    );
    assert_eq!(
        first.handle.snapshot().state().revision,
        shared_revision,
        "personal edits submit no shared command"
    );
    let a_saved = a.frame(first_id).personal.persisted().unwrap();
    let b_saved = b.frame(first_id).personal.persisted().unwrap();
    for desktop in [&a, &b] {
        let save = cx.update(|cx| desktop.provider.save_view(cx));
        settle_until(&executor, cx, || save.is_ready());
        cx.foreground_executor().block_test(save).unwrap();
    }
    let a_window = VisualTestContext::from_window(a.window, cx);
    let mut b_window = VisualTestContext::from_window(b.window, cx);
    let b_size = b_window.update(|window, _| window.viewport_size());
    a_window.simulate_resize(gpui::size(gpui::px(960.0), gpui::px(720.0)));
    drive(&executor, cx);
    assert_eq!(b_window.update(|window, _| window.viewport_size()), b_size);
    assert_eq!(a.frame(first_id).personal.persisted().unwrap(), a_saved);
    assert_eq!(b.frame(first_id).personal.persisted().unwrap(), b_saved);
    assert_eq!(first.handle.snapshot().state().revision, shared_revision);
    for (desktop, expected) in [(&a, &a_saved), (&b, &b_saved)] {
        assert_eq!(
            ViewPersistence::new(NativeViewStore::new(desktop.paths.clone()))
                .load()
                .unwrap()
                .as_ref(),
            Some(expected)
        );
    }
    let old_binding = match b
        .registry
        .server(first_id)
        .unwrap()
        .state()
        .borrow()
        .clone()
    {
        ConnectionState::Connected { info } => info.binding,
        state => panic!("connection before resync: {state:?}"),
    };
    executor
        .block_on(b.registry.server(first_id).unwrap().retry())
        .unwrap();
    settle_until(
        &executor,
        cx,
        || matches!(b.frame(first_id).connection, ConnectionState::Connected { info } if info.binding != old_binding),
    );
    assert_eq!(
        b.frame(first_id)
            .personal
            .workspace(workspace)
            .unwrap()
            .arrangement(tab)
            .unwrap(),
        &b_arrangement
    );
    let target = spaceterm_core::operations::CloseTarget::Tab(new_tab);
    let assessment = match a.shared(Command::AssessClose { target }, &executor) {
        Outcome::Assessment(assessment) => assessment.reference(),
        outcome => panic!("close assessment: {outcome:?}"),
    };
    assert_eq!(
        a.shared(Command::CloseConfirmed { target, assessment }, &executor),
        Outcome::Applied
    );
    settle_until(&executor, cx, || {
        b.frame(first_id)
            .personal
            .workspace(workspace)
            .unwrap()
            .tab_order()
            == [tab]
    });
    assert_eq!(
        a.frame(first_id)
            .personal
            .workspace(workspace)
            .unwrap()
            .active_tab(),
        Some(tab)
    );
    assert_eq!(b.frame(first_id).personal.active_workspace(), Some(other));
    assert_eq!(
        a.frame(first_id)
            .personal
            .workspace(workspace)
            .unwrap()
            .arrangement(tab)
            .unwrap(),
        &a_arrangement
    );
    assert_eq!(
        b.workspace_entity(workspace, cx).entity_id(),
        b_entity.entity_id()
    );
    assert_eq!(
        b_entity.read_with(cx, |tabs, _| tabs.server_tab(tab).unwrap().entity_id()),
        b_tab_entity.entity_id()
    );
    for server in [&first, &second] {
        assert!(
            server
                .handle
                .snapshot()
                .runtime()
                .iter()
                .all(|runtime| matches!(runtime.phase, SessionPhase::NotStarted)),
            "attachment, publication, focus and rendering cannot start shells"
        );
    }
    assert_eq!(
        a.frame(first_id).organization.state().revision,
        b.frame(first_id).organization.state().revision
    );
    for desktop in [&a, &b] {
        let quit = cx.update(|cx| desktop.provider.prepare_quit(cx));
        settle_until(&executor, cx, || quit.is_ready());
        cx.foreground_executor().block_test(quit).unwrap();
    }
    executor.block_on(first.shutdown());
    executor.block_on(second.shutdown());
}

#[gpui::test]
fn authenticated_desktop_settings_closure_and_native_reopen_preserve_ended_and_detached_state(
    cx: &mut TestAppContext,
) {
    use crate::ui::settings_window::{self, CloseSettingsWindow, SettingsWindow};
    let storage = crate::settings::storage::testing::MemoryStorage::with_document(
        &crate::settings::SettingsDocument::default(),
    );
    let settings = crate::settings::Settings::load(storage.clone());
    cx.update(|cx| {
        crate::ui::appearance_runtime::install(
            settings,
            Rc::new(crate::platform::appearance::testing::RecordingAppearancePlatform::default()),
            cx,
        )
        .unwrap();
        crate::ui::init(cx).unwrap();
        settings_window::init(cx);
        settings_window::configure_window_chrome(
            Rc::new(RecordingMovement),
            Default::default(),
            None,
            cx,
        );
    });
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let server = compatible_server(&executor, 2);
    let desktop = Desktop::new(&executor, &[&server], 0, cx);
    let server_id = server.handle.snapshot().state().server;
    desktop.wait_connected(server_id, &executor, cx);
    let (workspace, _, pane) = create_workspace(&desktop, server_id, &executor);
    desktop.wait_workspaces(server_id, 1, &executor, cx);
    desktop.request(DesktopRequest::AttachWorkspace(workspace), &executor, cx);
    desktop.request(
        DesktopRequest::StartSession {
            pane,
            source: crate::ui::server_desktop_contracts::SelectionSource::Pointer,
        },
        &executor,
        cx,
    );
    settle_until(&executor, cx, || {
        matches!(
            server.handle.snapshot().runtime()[0].phase,
            SessionPhase::Running { .. }
        )
    });
    let control = server.handle.snapshot().runtime()[0].control;
    cx.update(|cx| settings_window::open_or_activate(None, cx));
    drive(&executor, cx);
    let settings_window = cx.update(|cx| {
        cx.windows()
            .into_iter()
            .find(|window| window.downcast::<SettingsWindow>().is_some())
            .unwrap()
    });
    let assessment = match desktop.shared(
        Command::AssessEndAllSessions { server: server_id },
        &executor,
    ) {
        Outcome::Assessment(assessment) => assessment.reference(),
        outcome => panic!("End All assessment: {outcome:?}"),
    };
    assert_eq!(
        desktop.shared(
            Command::EndAllSessions {
                server: server_id,
                assessment
            },
            &executor
        ),
        Outcome::Applied
    );
    settle_until(&executor, cx, || {
        matches!(
            desktop.frame(server_id).organization.runtime()[0].phase,
            SessionPhase::Ended { .. }
        )
    });
    let ended = server.handle.snapshot().runtime()[0];
    assert_eq!(ended.control, control);
    let mut settings_context = VisualTestContext::from_window(settings_window, cx);
    settings_context.dispatch_action(CloseSettingsWindow);
    drive(&executor, cx);
    assert!(
        !cx.update(|cx| cx.windows().contains(&settings_window)),
        "the actual Settings window closes"
    );
    let mut workspace_context = VisualTestContext::from_window(desktop.window, cx);
    workspace_context.update(|window, cx| {
        window.activate_window();
        desktop
            .manager
            .update(cx, |manager, cx| manager.focus(window, cx));
    });
    workspace_context.simulate_resize(gpui::size(gpui::px(1100.0), gpui::px(800.0)));
    drive(&executor, cx);
    assert_eq!(
        server.handle.snapshot().runtime()[0],
        ended,
        "Settings closure, focus restoration and measurement cannot restart an ended session"
    );
    assert!(desktop.provider.pane_measurement(pane).is_none());

    // Save an attached document, then detach durably. Reopening the stale document must
    // consult the reopened native journal before exposing a lease or startup authority.
    let saved = desktop.frame(server_id).personal.persisted().unwrap();
    let save = cx.update(|cx| desktop.provider.save_view(cx));
    settle_until(&executor, cx, || save.is_ready());
    cx.foreground_executor().block_test(save).unwrap();
    desktop.request(DesktopRequest::DetachWorkspace(workspace), &executor, cx);
    assert!(
        desktop
            .frame(server_id)
            .personal
            .workspace(workspace)
            .is_none()
    );
    let reopened_journal = executor.block_on(async {
        let codec = Arc::new(spaceterm_core::operation_ledger::canonical::CanonicalCodec);
        let native = Arc::new(
            NativeClientJournal::open_existing(
                desktop.journal_path.clone(),
                codec.clone(),
                Arc::new(Verifier),
                JournalLimits::default(),
            )
            .await
            .unwrap(),
        );
        JournalController::recover(native, codec, Arc::new(Verifier), JournalLimits::default())
            .await
            .unwrap()
    });
    let mut persistence = ViewPersistence::new(NativeViewStore::new(desktop.paths.clone()));
    assert_eq!(persistence.load().unwrap(), Some(saved));
    let (restored, _) =
        spaceterm_client::view::launch::restore(&mut persistence, &reopened_journal, &[]).unwrap();
    assert!(
        restored.attachments().is_empty(),
        "durable suppression wins over a stale view file"
    );
    assert_eq!(
        desktop.journal.snapshot().unwrap().revision(),
        reopened_journal.snapshot().unwrap().revision()
    );
    executor.block_on(server.shutdown());
    settle_until(&executor, cx, || {
        matches!(
            desktop.frame(server_id).connection,
            ConnectionState::Offline { .. }
        )
    });
    let view_before_settings = desktop.frame(server_id).personal.persisted().unwrap();
    cx.update(|cx| settings_window::open_or_activate(None, cx));
    drive(&executor, cx);
    let offline_settings = cx.update(|cx| {
        cx.windows()
            .into_iter()
            .find(|window| window.downcast::<SettingsWindow>().is_some())
            .unwrap()
    });
    let mut offline_context = VisualTestContext::from_window(offline_settings, cx);
    let density = offline_context
        .debug_bounds("settings-density-comfortable")
        .unwrap();
    offline_context.simulate_click(density.center(), gpui::Modifiers::none());
    offline_context.dispatch_action(CloseSettingsWindow);
    drive(&executor, cx);
    assert!(!cx.update(|cx| cx.windows().contains(&offline_settings)));
    assert_eq!(
        storage.document().unwrap().appearance.window.density,
        crate::appearance::ChromeDensity::Comfortable
    );
    assert_eq!(
        desktop.frame(server_id).personal.persisted().unwrap(),
        view_before_settings,
        "ordinary Settings do not reset native attachments"
    );
    assert!(!cx.update(|cx| desktop.runtime.read(cx).has_pending_local_requests()));
    let quit = cx.update(|cx| desktop.provider.prepare_quit(cx));
    settle_until(&executor, cx, || quit.is_ready());
    cx.foreground_executor().block_test(quit).unwrap();
}

#[gpui::test]
fn authenticated_desktop_native_geometry_and_offline_settings_preserve_client_documents(
    cx: &mut TestAppContext,
) {
    use crate::ui::settings_window::{self, CloseSettingsWindow, SettingsWindow};
    let storage = crate::settings::storage::testing::MemoryStorage::with_document(
        &crate::settings::SettingsDocument::default(),
    );
    let settings = crate::settings::Settings::load(storage.clone());
    cx.update(|cx| {
        crate::ui::appearance_runtime::install(
            settings,
            Rc::new(crate::platform::appearance::testing::RecordingAppearancePlatform::default()),
            cx,
        )
        .unwrap();
        crate::ui::init(cx).unwrap();
        settings_window::init(cx);
        settings_window::configure_window_chrome(
            Rc::new(RecordingMovement),
            Default::default(),
            None,
            cx,
        );
    });
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let first = compatible_server(&executor, 2);
    let second = compatible_server(&executor, 1);
    let a = Desktop::new(&executor, &[&first, &second], 0, cx);
    let b = Desktop::new(&executor, &[&first, &second], 1, cx);
    let first_id = first.handle.snapshot().state().server;
    let second_id = second.handle.snapshot().state().server;
    for desktop in [&a, &b] {
        for server in [first_id, second_id] {
            desktop.wait_connected(server, &executor, cx);
        }
        let represented = cx.update(|cx| {
            desktop
                .runtime
                .read(cx)
                .servers()
                .iter()
                .map(|server| server.server)
                .collect::<Vec<_>>()
        });
        assert_eq!(represented.len(), 2);
        assert!(represented.contains(&first_id) && represented.contains(&second_id));
    }
    assert_ne!(a.runtime.entity_id(), b.runtime.entity_id());
    for server in [first_id, second_id] {
        let (ConnectionState::Connected { info: left }, ConnectionState::Connected { info: right }) = (
            a.frame(server).connection.clone(),
            b.frame(server).connection.clone(),
        ) else {
            panic!("authenticated clients")
        };
        assert_eq!(left.server, server);
        assert_eq!(right.server, server);
        assert_ne!(left.client, right.client);
        assert_eq!(
            a.frame(server).organization.state().revision,
            b.frame(server).organization.state().revision
        );
    }
    let a_document = a.frame(first_id).personal.persisted().unwrap();
    let b_document = b.frame(first_id).personal.persisted().unwrap();
    let a_window = VisualTestContext::from_window(a.window, cx);
    let mut b_window = VisualTestContext::from_window(b.window, cx);
    let b_size = b_window.update(|window, _| window.viewport_size());
    let revisions = [
        first.handle.snapshot().state().revision,
        second.handle.snapshot().state().revision,
    ];
    a_window.simulate_resize(gpui::size(gpui::px(960.0), gpui::px(720.0)));
    drive(&executor, cx);
    assert_eq!(b_window.update(|window, _| window.viewport_size()), b_size);
    assert_eq!(a.frame(first_id).personal.persisted().unwrap(), a_document);
    assert_eq!(b.frame(first_id).personal.persisted().unwrap(), b_document);
    assert_eq!(
        [
            first.handle.snapshot().state().revision,
            second.handle.snapshot().state().revision
        ],
        revisions
    );
    executor.block_on(first.shutdown());
    settle_until(&executor, cx, || {
        matches!(
            a.frame(first_id).connection,
            ConnectionState::Offline { .. }
        )
    });
    assert!(matches!(
        a.frame(second_id).connection,
        ConnectionState::Connected { .. }
    ));
    let journal_revision = a.journal.snapshot().unwrap().revision();
    cx.update(|cx| settings_window::open_or_activate(None, cx));
    drive(&executor, cx);
    let settings_window = cx.update(|cx| {
        cx.windows()
            .into_iter()
            .find(|window| window.downcast::<SettingsWindow>().is_some())
            .unwrap()
    });
    let mut settings_context = VisualTestContext::from_window(settings_window, cx);
    let density = settings_context
        .debug_bounds("settings-density-comfortable")
        .unwrap();
    settings_context.simulate_click(density.center(), gpui::Modifiers::none());
    settings_context.dispatch_action(CloseSettingsWindow);
    drive(&executor, cx);
    assert!(!cx.update(|cx| cx.windows().contains(&settings_window)));
    assert_eq!(
        storage.document().unwrap().appearance.window.density,
        crate::appearance::ChromeDensity::Comfortable
    );
    assert_eq!(a.frame(first_id).personal.persisted().unwrap(), a_document);
    assert_eq!(a.journal.snapshot().unwrap().revision(), journal_revision);
    for desktop in [&a, &b] {
        let quit = cx.update(|cx| desktop.provider.prepare_quit(cx));
        settle_until(&executor, cx, || quit.is_ready());
        cx.foreground_executor().block_test(quit).unwrap();
    }
    assert_eq!(
        ViewPersistence::new(NativeViewStore::new(a.paths.clone()))
            .load()
            .unwrap(),
        Some(a_document)
    );
    let reopened = executor.block_on(async {
        let codec = Arc::new(spaceterm_core::operation_ledger::canonical::CanonicalCodec);
        let native = Arc::new(
            NativeClientJournal::open_existing(
                a.journal_path.clone(),
                codec.clone(),
                Arc::new(Verifier),
                JournalLimits::default(),
            )
            .await
            .unwrap(),
        );
        JournalController::recover(native, codec, Arc::new(Verifier), JournalLimits::default())
            .await
            .unwrap()
    });
    assert_eq!(reopened.snapshot().unwrap().bindings().len(), 2);
    for binding in &a.bindings {
        assert!(
            reopened
                .snapshot()
                .unwrap()
                .bindings()
                .iter()
                .any(|saved| saved.binding().unwrap() == *binding)
        );
    }
    executor.block_on(second.shutdown());
}
