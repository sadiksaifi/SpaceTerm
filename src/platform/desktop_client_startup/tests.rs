use super::*;
use spaceterm_client::{
    api::{ConnectedInfo, ConnectionTarget, PairedServerBinding},
    failure::{ClientFailure, ClientFailureClass},
    journal::{ClientJournalChange, JournalAttachment},
    limits::ClientLimits,
    ports::*,
    projection::ProjectionBudgets,
    replica::ReplicaSnapshot,
    testing::{FakePaneConnection, FakeServer},
    view::{
        self, EncodedView, StoredView, ViewCommit, ViewStoreFailure, ViewStoreFailureClass,
        ViewStoreStage,
    },
};
use spaceterm_core::{
    authority::GrantedControl,
    identity::WorkspaceId,
    operations::{Command, WorkspaceCreationSource},
    organization::{CreationTopologyReadModel, NameDraft},
    pairing::LocalPairingKey,
};
use spaceterm_protocol::codec::Payload;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Default)]
struct OfflinePorts {
    opens: AtomicUsize,
}
impl RouteConnector for OfflinePorts {
    fn open<'a>(
        &'a self,
        _: &'a ConnectionTarget,
        _: ConnectionPurpose,
    ) -> ClientFuture<'a, ConnectedRoute> {
        self.opens.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(ClientFailure::new(ClientFailureClass::Unavailable)) })
    }
}
impl ConnectionJournal for OfflinePorts {
    fn recover_bindings(&self) -> ClientFuture<'_, Vec<PairedServerBinding>> {
        Box::pin(async { panic!("second connection journal used") })
    }
    fn ordinary_use_allowed<'a>(&'a self, _: &'a PairedServerBinding) -> ClientFuture<'a, bool> {
        Box::pin(async { panic!("second connection journal used") })
    }
}
impl OrganizationDecoder for OfflinePorts {
    fn decode(&self, _: Payload) -> Result<OrganizationPublication, ClientFailure> {
        panic!("offline recovery decoded a publication")
    }
}
impl ConnectionDelegate for OfflinePorts {
    fn invalidate(&self, _: &PairedServerBinding, _: ConnectionInvalidation) {}
    fn reconcile<'a>(
        &'a self,
        _: ConnectedInfo,
        _: &'a ReplicaSnapshot,
        _: &'a mut dyn ProtocolChannel,
    ) -> ClientFuture<'a, ()> {
        Box::pin(async { panic!("offline recovery reconciled") })
    }
    fn publication(&self, _: ConnectedInfo, _: &ReplicaSnapshot) -> Result<(), ClientFailure> {
        panic!("offline recovery published")
    }
    fn admit(&self, _: ConnectedInfo, _: &Payload) -> Result<OutboundClass, ClientFailure> {
        panic!("offline recovery admitted effects")
    }
    fn receive<'a>(
        &'a self,
        _: ConnectedInfo,
        _: Payload,
        _: &'a mut dyn ProtocolChannel,
    ) -> ClientFuture<'a, ()> {
        Box::pin(async { panic!("offline recovery received effects") })
    }
}

struct Invalidator(Arc<PaneChannels>);
impl LocalUseInvalidator for Invalidator {
    fn invalidate_workspace(
        &self,
        _: &LocalPairingKey,
        _: WorkspaceId,
    ) -> Result<Vec<GrantedControl>, ClientFailure> {
        panic!("recovery detached work")
    }
    fn invalidate_attachment(
        &self,
        pairing: &LocalPairingKey,
        attachment: view::WorkspaceAttachment,
    ) -> Result<Vec<GrantedControl>, ClientFailure> {
        self.0.invalidate_attachment(pairing, attachment)
    }
    fn invalidate_pairing(&self, _: &LocalPairingKey) {
        panic!("recovery unpaired a device")
    }
    fn remove_pairing<'a>(&'a self, _: &'a LocalPairingKey) -> ClientFuture<'a, ()> {
        Box::pin(async { panic!("recovery removed a pairing") })
    }
}
pub(super) struct MemoryView {
    pub(super) bytes: Option<Vec<u8>>,
    pub(super) failure: Option<ViewStoreFailureClass>,
    pub(super) writes: Arc<AtomicUsize>,
    pub(super) saved: Arc<std::sync::Mutex<Option<Vec<u8>>>>,
}
impl ViewStore for MemoryView {
    type Version = u64;
    fn load(&mut self, _: usize) -> Result<Option<StoredView<u64>>, ViewStoreFailure> {
        if let Some(class) = self.failure {
            return Err(ViewStoreFailure {
                class,
                stage: ViewStoreStage::Load,
            });
        }
        Ok(self
            .bytes
            .clone()
            .map(|bytes| StoredView { version: 1, bytes }))
    }
    fn replace(&mut self, expected: Option<&u64>, document: &EncodedView) -> ViewCommit<u64> {
        assert_eq!(expected.copied(), self.bytes.as_ref().map(|_| 1));
        self.writes.fetch_add(1, Ordering::SeqCst);
        *self.saved.lock().unwrap() = Some(document.as_bytes().to_vec());
        ViewCommit::Durable(2)
    }
}

struct Fixture {
    server: Arc<FakeServer>,
    journal: Arc<JournalController>,
    channels: Arc<PaneChannels>,
    view: view::ClientViewState,
    ports: Arc<OfflinePorts>,
    writes: Arc<AtomicUsize>,
}
impl Fixture {
    async fn new() -> Self {
        let server = FakeServer::new(1, vec![2, 1]).unwrap();
        let peer = FakePaneConnection::new(server.clone(), 1, 2).await.unwrap();
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
        let snapshot = server.replica().unwrap();
        let info = server.connected_info(1, 1, 1, 2).unwrap();
        let binding = server.binding(1).unwrap();
        let observation = view::ReplicaObservation::new(
            &snapshot.organization,
            snapshot.accepted_creations(),
            view::ReplicaFacts {
                pairing: binding.target().pairing(),
                client: info.client,
                connection: Some(info.binding),
                readiness: view::ReplicaReadiness::Ready,
                status: view::ServerViewStatus::Connected,
            },
        )
        .unwrap();
        let mut view = view::ClientViewState::new();
        let workspace = snapshot.organization.state().workspaces[0].id;
        view.attach(workspace, &observation).unwrap();
        let journal = peer.journal().clone();
        journal
            .record_attachment(
                binding.target().local_pairing().unwrap(),
                view.workspace(workspace).unwrap().attachment(),
            )
            .await
            .unwrap();
        let channels = peer
            .channels(ProjectionBudgets::supported().unwrap())
            .unwrap();
        Self {
            server,
            journal,
            channels,
            view,
            ports: Arc::default(),
            writes: Arc::default(),
        }
    }
    fn owners(&self) -> NativeDesktopOwners {
        NativeDesktopOwners {
            services: ClientServices {
                routes: self.ports.clone(),
                credentials: self.server.clone(),
                journal: self.ports.clone(),
                organization: self.ports.clone(),
                delegate: self.ports.clone(),
                discovery: None,
                limits: ClientLimits::default(),
                additional_capabilities: vec![],
            },
            journal: self.journal.clone(),
            channels: self.channels.clone(),
            operations: self.server.clone(),
            invalidator: Arc::new(Invalidator(self.channels.clone())),
            local_server: Some(self.server.server()),
            tracker: None,
            registry_binding: None,
        }
    }
    fn store(&self) -> MemoryView {
        MemoryView {
            bytes: Some(
                self.view
                    .persisted()
                    .unwrap()
                    .encode()
                    .unwrap()
                    .into_bytes(),
            ),
            failure: None,
            writes: self.writes.clone(),
            saved: Arc::default(),
        }
    }
}

#[test]
fn desktop_recovery_restores_saved_attachments_before_any_connection_or_startup() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let fixture = Fixture::new().await;
        let composition = recover(fixture.owners(), fixture.store()).await.unwrap();
        assert_eq!(composition.view.attachments(), fixture.view.attachments());
        assert_eq!(composition.servers.len(), 1);
        assert!(composition.servers[0].local);
        assert_eq!(composition.registry.servers().len(), 1);
        assert!(Arc::ptr_eq(
            composition.tracker.journal(),
            composition.pending.journal()
        ));
        assert!(Arc::ptr_eq(composition.tracker.journal(), &fixture.journal));
        assert_eq!(fixture.ports.opens.load(Ordering::SeqCst), 0);
        assert_eq!(fixture.writes.load(Ordering::SeqCst), 0);
        composition.runtime.shutdown().await.unwrap();
    });
}

#[test]
fn desktop_recovery_applies_durable_detach_before_materializing_saved_attachment() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let fixture = Fixture::new().await;
        let attachment = fixture.view.attachments()[0];
        fixture
            .journal
            .transact(vec![ClientJournalChange::SuppressAttachment(
                JournalAttachment::new(
                    fixture
                        .server
                        .binding(1)
                        .unwrap()
                        .target()
                        .local_pairing()
                        .unwrap(),
                    attachment,
                )
                .unwrap(),
            )])
            .await
            .unwrap();
        let composition = recover(fixture.owners(), fixture.store()).await.unwrap();
        assert!(composition.view.attachments().is_empty());
        assert_eq!(fixture.ports.opens.load(Ordering::SeqCst), 0);
        assert_eq!(fixture.writes.load(Ordering::SeqCst), 0);
        composition.runtime.shutdown().await.unwrap();
    });
}

#[test]
fn desktop_recovery_locked_or_unavailable_view_never_becomes_empty_writable_defaults() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        for failure in [
            ViewStoreFailureClass::Locked,
            ViewStoreFailureClass::Unavailable,
            ViewStoreFailureClass::Unsafe,
        ] {
            let fixture = Fixture::new().await;
            let mut store = fixture.store();
            store.failure = Some(failure);
            let error = match recover(fixture.owners(), store).await {
                Ok(_) => panic!("unreadable view admitted"),
                Err(error) => error,
            };
            assert_eq!(error, DesktopClientError::LocalStorageUnavailable);
            assert_eq!(fixture.ports.opens.load(Ordering::SeqCst), 0);
            assert_eq!(fixture.writes.load(Ordering::SeqCst), 0);
        }
    });
}

#[test]
fn desktop_recovery_corrupt_view_is_preserved_without_replacement_server_work() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let fixture = Fixture::new().await;
        let mut store = fixture.store();
        store.bytes = Some(b"invalid retained view".to_vec());
        assert!(recover(fixture.owners(), store).await.is_err());
        assert_eq!(fixture.ports.opens.load(Ordering::SeqCst), 0);
        assert_eq!(fixture.writes.load(Ordering::SeqCst), 0);
    });
}

#[gpui::test]
async fn desktop_install_rechecks_detach_committed_after_native_view_recovery(
    cx: &mut gpui::TestAppContext,
) {
    let tokio = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let fixture = tokio.block_on(Fixture::new());
    let store = fixture.store();
    let saved = store.saved.clone();
    let composition = tokio.block_on(recover(fixture.owners(), store)).unwrap();
    let attachment = fixture.view.attachments()[0];
    tokio
        .block_on(
            fixture
                .journal
                .transact(vec![ClientJournalChange::SuppressAttachment(
                    JournalAttachment::new(
                        fixture
                            .server
                            .binding(1)
                            .unwrap()
                            .target()
                            .local_pairing()
                            .unwrap(),
                        attachment,
                    )
                    .unwrap(),
                )]),
        )
        .unwrap();
    let runtime = cx.update(|cx| composed::install(composition, cx).unwrap());
    let save = runtime.update(cx, |runtime, cx| runtime.save_view(cx));
    save.await.unwrap();
    let document = saved.lock().unwrap().clone().unwrap();
    let view = view::PersistedView::decode(&document).unwrap();
    assert!(view.parts().workspaces.is_empty());
    assert_eq!(fixture.ports.opens.load(Ordering::SeqCst), 0);
}

#[gpui::test]
async fn desktop_native_startup_factory_consumes_recovered_native_owners_once(
    cx: &mut gpui::TestAppContext,
) {
    // This test waits on the actual native executor, not a GPUI-scheduled test task.
    cx.executor().allow_parking();
    let executor = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap(),
    );
    let fixture = executor.block_on(Fixture::new());
    let before = fixture.journal.snapshot().unwrap().revision();
    let startup = NativeDesktopStartup::new(fixture.owners(), fixture.store(), executor.clone());
    assert_eq!(fixture.ports.opens.load(Ordering::SeqCst), 0);
    let recovered = cx.update(|cx| startup.recover(cx)).await.unwrap();
    assert_eq!(fixture.ports.opens.load(Ordering::SeqCst), 0);
    let second = cx.update(|cx| startup.recover(cx)).await;
    assert!(matches!(second, Err(DesktopClientError::Cancelled)));
    let runtime = cx.update(|cx| recovered.install(cx).unwrap());
    assert_eq!(cx.update(|cx| runtime.read(cx).servers().len()), 1);
    assert_eq!(fixture.journal.snapshot().unwrap().revision(), before);
    assert_eq!(fixture.writes.load(Ordering::SeqCst), 0);
}

#[test]
fn desktop_recovery_keeps_two_saved_servers_and_their_attachments_independent() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut fixture = Fixture::new().await;
        let second = FakeServer::new(7, vec![2, 1]).unwrap();
        let peer = FakePaneConnection::new(second.clone(), 1, 2).await.unwrap();
        peer.submit(
            Command::CreateWorkspace {
                server: second.server(),
                source: WorkspaceCreationSource::ServerHome {
                    server: second.server(),
                },
                name: NameDraft::Automatic,
            },
            None,
        )
        .await
        .unwrap();
        let saved = peer.journal().snapshot().unwrap().bindings()[0].clone();
        fixture
            .journal
            .transact(vec![ClientJournalChange::SaveBinding(Box::new(saved))])
            .await
            .unwrap();
        let snapshot = second.replica().unwrap();
        let info = second.connected_info(1, 1, 1, 2).unwrap();
        let binding = second.binding(1).unwrap();
        let observation = view::ReplicaObservation::new(
            &snapshot.organization,
            snapshot.accepted_creations(),
            view::ReplicaFacts {
                pairing: binding.target().pairing(),
                client: info.client,
                connection: Some(info.binding),
                readiness: view::ReplicaReadiness::Ready,
                status: view::ServerViewStatus::Connected,
            },
        )
        .unwrap();
        let workspace = snapshot.organization.state().workspaces[0].id;
        fixture.view.attach(workspace, &observation).unwrap();
        fixture
            .journal
            .record_attachment(
                binding.target().local_pairing().unwrap(),
                fixture.view.workspace(workspace).unwrap().attachment(),
            )
            .await
            .unwrap();
        let composition = recover(fixture.owners(), fixture.store()).await.unwrap();
        assert_eq!(composition.view.attachments(), fixture.view.attachments());
        assert_eq!(composition.registry.servers().len(), 2);
        assert!(
            composition
                .registry
                .server(fixture.server.server())
                .is_some()
        );
        assert!(composition.registry.server(second.server()).is_some());
        assert_eq!(
            composition
                .servers
                .iter()
                .filter(|server| server.local)
                .count(),
            1
        );
        assert_eq!(fixture.ports.opens.load(Ordering::SeqCst), 0);
        composition.runtime.shutdown().await.unwrap();
    });
}

#[gpui::test]
fn desktop_application_composes_native_directory_ports_for_all_paired_servers(
    cx: &mut gpui::TestAppContext,
) {
    use crate::ui::server_client_runtime::DesktopClientProvider as _;
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let fixture = executor.block_on(Fixture::new());
    let other = FakeServer::new(9, vec![2, 1]).unwrap();
    let peer = executor
        .block_on(FakePaneConnection::new(other.clone(), 1, 2))
        .unwrap();
    let saved = peer.journal().snapshot().unwrap().bindings()[0].clone();
    executor
        .block_on(
            fixture
                .journal
                .transact(vec![ClientJournalChange::SaveBinding(Box::new(saved))]),
        )
        .unwrap();
    let composition = executor
        .block_on(recover(fixture.owners(), fixture.store()))
        .unwrap();
    let provider = cx.update(|cx| {
        let (notifier, _) = crate::ui::server_client_runtime::notification_channel();
        crate::ui::server_client_runtime::composed::ComposedDesktopClientProvider::new(
            composition,
            notifier,
            cx,
        )
        .unwrap()
    });
    for server in [fixture.server.server(), other.server()] {
        let port = provider
            .directory_connection(server)
            .expect("paired server directory port");
        // Ports exist while offline, but never manufacture authenticated authority.
        assert!(port.state().is_err());
    }
    assert!(
        provider
            .directory_connection(spaceterm_core::identity::ServerId::new([99; 16]))
            .is_err()
    );
    assert_eq!(fixture.ports.opens.load(Ordering::SeqCst), 0);
}
